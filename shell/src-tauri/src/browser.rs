//! §spec browser-integrado
//!
//! The integrated browser, owner side: every tab is a child webview of the main window, positioned
//! over the `/web` page's content area by the React side. The `owner` environment has no CDP; the
//! `agent` environment is built by `browser_env_ensure` (proxy, CDP port, hidden keepalive webview).
//!
//! **Browser webviews hold no Tauri IPC.** `capabilities/default.json` is scoped to the window
//! `main` and has no `remote` section, so no capability reaches a remote origin. This app also ships
//! no ACL manifest for its own commands, so the only thing standing between a page and these very
//! commands is tauri's rule that a command called from a non-local origin is checked against the ACL
//! (tauri 2.12.1 `webview/mod.rs`, the `has_app_acl_manifest || !is_local` guard) and, with no
//! remote capability, refused. That rule keys on `Webview::is_local_url`, which counts the app's own
//! origins as local: `tauri://`, `*.localhost` custom-protocol hosts (`tauri.localhost` on Windows)
//! and the `devUrl`. [`navigation_allowed`] therefore keeps a browser tab off all of them; without
//! that, typing `http://tauri.localhost/` would hand a tab the whole app's commands.
//!
//! **Threading.** `Window::add_child` blocks on `run_on_main_thread` and waits for the answer, so
//! calling it from the main thread deadlocks. Plain `#[tauri::command]` functions run on the main
//! thread; `browser_tab_create` is therefore `async` and does the build on `spawn_blocking`, never
//! on the main thread. Every other command only queues a message to the main thread and returns.
//!
//! The decisions (secure flag, labels, environment keys, data directories, browser arguments,
//! bounds, the registry, what is shown) are pure functions and plain structs, tested below; what
//! remains is thin glue over the tauri runtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::browser_agent::{
    agent_args, free_loopback_port, generation_dir_name, is_keepalive_tab_id, keepalive_label_for,
    keepalive_tab_id, rebuild_decision, RebuildDecision,
};
use serde::{Deserialize, Serialize};
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Rect, Size, Url,
    WebviewUrl,
};

/// Every event goes to the main window only: a browser tab's URL and title are the owner's
/// business, and a broadcast would offer them to the pages themselves.
const MAIN_LABEL: &str = "main";

/// `{tabId, url, title, loading, canGoBack, canGoForward, secure}`, on every navigation, title or
/// load change.
const TAB_STATE_EVENT: &str = "browser://tab-state";
/// `{fromTabId, url}`: a page asked for a new window; it was denied and the shell opens a tab.
const NEW_WINDOW_EVENT: &str = "browser://new-window";
/// `{tabId, kind}`: a WebView2 process of that tab died.
#[cfg_attr(not(windows), allow(dead_code))]
const PROCESS_FAILED_EVENT: &str = "browser://process-failed";
/// `{}`: the WebView2 runtime is not installed.
const RUNTIME_MISSING_EVENT: &str = "browser://runtime-missing";
/// `{project, role, closedTabIds}`: the agent environment was rebuilt because `proxyAddr` changed;
/// the listed tabs were closed (the keepalive webview is not among them).
const ENV_REBUILT_EVENT: &str = "browser://env-rebuilt";

/// A tab's webview label is this prefix followed by the tab id the React side minted.
const LABEL_PREFIX: &str = "browser-";

/// What wry passes as browser arguments when nobody sets any (wry 0.57 `webview2/mod.rs`): it
/// removes the "mini menu" and SmartScreen. `additional_browser_args` REPLACES this string, so it
/// is restored here and everything extra is appended to it.
///
/// wry also adds `--autoplay-policy=no-user-gesture-required` while its `autoplay` attribute is on
/// (its default). It is left out on purpose: a browser tab should not autoplay media without a
/// gesture, and the main window, which does keep it, is a different environment.
const DEFAULT_BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

// -------------------------------------------------------------------------------------------------
// Pure decisions
// -------------------------------------------------------------------------------------------------

/// Which party a browser environment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Agent,
}

impl Role {
    fn parse(word: &str) -> Result<Role, String> {
        match word {
            "owner" => Ok(Role::Owner),
            "agent" => Ok(Role::Agent),
            _ => Err("invalid_role".to_string()),
        }
    }

    /// The word the data directory is named by.
    fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Agent => "agent",
        }
    }
}

/// An environment is one WebView2 data directory with one fixed argument string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvKey {
    project: String,
    role: Role,
}

impl EnvKey {
    /// Validates both halves; the project becomes a directory name, so it must be path-safe.
    fn new(project: &str, role: &str) -> Result<EnvKey, String> {
        let role = Role::parse(role)?;
        check_project(project)?;
        Ok(EnvKey {
            project: project.to_string(),
            role,
        })
    }

    // RED stub: wrong constant on purpose, the real no-project scope is not implemented yet.
    #[allow(dead_code)]
    fn none(role: Role) -> EnvKey {
        EnvKey {
            project: "p".to_string(),
            role,
        }
    }

    // RED stub: ignores the scope on purpose.
    #[allow(dead_code)]
    fn scoped(_project: Option<&str>, role: Role) -> EnvKey {
        EnvKey::none(role)
    }
}

// RED stub
#[allow(dead_code)]
fn should_retire_none_agent(_registry: &Registry, _key: &EnvKey) -> bool {
    todo!()
}

// RED stub
#[allow(dead_code)]
fn stale_none_agent_dirs(_names: &[String], _live_generation: Option<u32>) -> Vec<String> {
    todo!()
}

/// Names Windows will not let a directory carry, whatever the extension.
const RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A project id is used as one directory name, so anything that could leave that directory, or
/// that the file system would read differently, is refused rather than rewritten: two projects
/// must never end up sharing a profile because their ids sanitised to the same word.
fn check_project(project: &str) -> Result<(), String> {
    let bad = project.is_empty()
        || project.chars().count() > 128
        || project.contains("..")
        || project.starts_with('.')
        || project.ends_with('.')
        || project.starts_with(' ')
        || project.ends_with(' ')
        || project
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c));
    let stem = project
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if bad || RESERVED_NAMES.contains(&stem.as_str()) {
        return Err("invalid_project".to_string());
    }
    Ok(())
}

/// A tab id ends up inside a webview label, which tauri restricts to a small alphabet.
fn check_tab_id(tab_id: &str) -> Result<(), String> {
    let ok = !tab_id.is_empty()
        && !is_keepalive_tab_id(tab_id)
        && tab_id.len() <= 64
        && tab_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err("invalid_tab_id".to_string())
    }
}

fn label_for(tab_id: &str) -> String {
    format!("{LABEL_PREFIX}{tab_id}")
}

/// The tab id behind a webview label, or `None` for any webview that is not a browser tab (the
/// main one, the notch).
fn tab_id_of_label(label: &str) -> Option<&str> {
    label.strip_prefix(LABEL_PREFIX).filter(|id| !id.is_empty())
}

/// `%LOCALAPPDATA%\nucleos\browser\<project>\<role>`; `base` is the local data directory.
fn data_dir(base: &Path, key: &EnvKey) -> PathBuf {
    base.join("nucleos")
        .join("browser")
        .join(&key.project)
        .join(key.role.as_str())
}

/// The restored defaults plus `extra`, in the order given. Deterministic: WebView2 refuses a second
/// webview on a data directory whose arguments differ from the first's, so the string built for an
/// environment must be byte-for-byte the same every time. The owner environment passes no extras.
fn browser_args(extra: &[&str]) -> String {
    let mut args = String::from(DEFAULT_BROWSER_ARGS);
    for arg in extra {
        args.push(' ');
        args.push_str(arg);
    }
    args
}

/// Only an `https` page is `secure` (the padlock); `http`, `about:blank` and the rest are not.
fn is_secure(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.scheme() == "https")
}

/// Whether a tab may navigate to `url`: web pages and `about:blank`, and never the app's own
/// origins (see the module docs). `dev_url` is `build.devUrl` from the tauri config.
fn navigation_allowed(url: &Url, dev_url: Option<&Url>) -> bool {
    match url.scheme() {
        "http" | "https" => {}
        "about" => return url.as_str().starts_with("about:blank"),
        _ => return false,
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    // `tauri.localhost`, `ipc.localhost`, and every custom protocol on Windows live under here.
    if host.ends_with(".localhost") {
        return false;
    }
    // `localhost:3000` is a dev server the owner wants to browse; only the app's own dev origin is not.
    !dev_url.is_some_and(|dev| dev.origin() == url.origin())
}

/// Whether an iframe may navigate to `raw`. Stricter than nothing, laxer than
/// [`navigation_allowed`]: `about:srcdoc`, `blob:` and `data:` frames are everyday web content and
/// cannot reach the app's origin, so they pass; an unparsable URI or any app origin does not.
#[cfg_attr(not(windows), allow(dead_code))]
fn frame_navigation_allowed(raw: &str, dev_url: Option<&Url>) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "about" | "blob" | "data" => true,
        _ => navigation_allowed(&url, dev_url),
    }
}

/// Whether `url` points at this machine: `localhost`, `*.localhost`, 127.0.0.0/8, `::1` (also as an
/// IPv4-mapped address) or the unspecified address. Chromium never sends loopback through a proxy
/// unless told to, so the fence proxy cannot see such requests.
fn is_loopback_url(url: &Url) -> bool {
    use std::net::IpAddr;
    let Some(host) = url.host_str() else {
        return false;
    };
    // `Url` already normalises decimal/hex/octal IPv4 forms; IPv6 comes bracketed.
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
        Err(_) => host == "localhost" || host.ends_with(".localhost"),
    }
}

/// [`navigation_allowed`], plus: an agent tab never reaches loopback hosts (user tabs may).
fn navigation_allowed_for(url: &Url, dev_url: Option<&Url>, agent: bool) -> bool {
    navigation_allowed(url, dev_url) && !(agent && is_loopback_url(url))
}

/// A position and size in logical pixels, relative to the main window's client area.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Bounds {
    fn validate(&self) -> Result<(), String> {
        let finite = [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite());
        if finite && self.width > 0.0 && self.height > 0.0 {
            Ok(())
        } else {
            Err("invalid_bounds".to_string())
        }
    }

    fn rect(&self) -> Rect {
        Rect {
            position: Position::Logical(LogicalPosition::new(self.x, self.y)),
            size: Size::Logical(LogicalSize::new(self.width, self.height)),
        }
    }
}

/// What to show: for every browser tab, whether it is visible. `None` hides all; a target that is
/// not one of `tabs` is an error, so a stale id cannot silently blank the page.
fn plan_show(tabs: &[String], target: Option<&str>) -> ShowPlan {
    let known = match target {
        Some(target) => tabs.iter().any(|t| t == target),
        None => true,
    };
    ShowPlan {
        // An unknown target shows nothing, so every tab is hidden before the error is reported.
        visibility: tabs
            .iter()
            .map(|t| (t.clone(), known && Some(t.as_str()) == target))
            .collect(),
        error: (!known).then(|| "unknown_tab".to_string()),
    }
}

/// The outcome of [`plan_show`]: apply `visibility` first, then report `error` if any.
#[derive(Debug, PartialEq, Eq)]
struct ShowPlan {
    visibility: Vec<(String, bool)>,
    error: Option<String>,
}

/// A WebView2 `COREWEBVIEW2_PROCESS_FAILED_KIND` as the word the shell shows.
#[cfg_attr(not(windows), allow(dead_code))]
fn process_failed_kind(kind: i32) -> &'static str {
    match kind {
        0 => "browser_process_exited",
        1 => "render_process_exited",
        2 => "render_process_unresponsive",
        3 => "frame_render_process_exited",
        4 => "utility_process_exited",
        5 => "sandbox_helper_process_exited",
        6 => "gpu_process_exited",
        7 => "ppapi_plugin_process_exited",
        8 => "ppapi_broker_process_exited",
        _ => "unknown_process_exited",
    }
}

/// Best effort: whether a failure to build a webview says "there is no WebView2 runtime". wry
/// reports it as a raw HRESULT (`0x80070002`, file not found) or with the runtime named.
fn looks_like_runtime_missing(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("80070002")
        || (m.contains("webview2")
            && (m.contains("runtime") || m.contains("not installed") || m.contains("not found")))
}

// -------------------------------------------------------------------------------------------------
// Registry
// -------------------------------------------------------------------------------------------------

/// What an environment was created with. Fixed at first creation: the arguments cannot change
/// while any webview still uses the data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvInfo {
    args: String,
    data_dir: PathBuf,
    /// The agent environment's proxy; `None` for the owner.
    proxy_addr: Option<String>,
    /// The agent environment's CDP port; `None` for the owner.
    cdp_port: Option<u16>,
    /// Which `agent[-n]` data directory this build uses; 0 for the owner and a first agent build.
    generation: u32,
}

/// Environments by key, and which environment each tab belongs to. No tauri types: testable bare.
#[derive(Debug, Default)]
pub struct Registry {
    envs: HashMap<EnvKey, EnvInfo>,
    tabs: HashMap<String, EnvKey>,
    /// Tabs whose webview is being built, with whether a close arrived meanwhile.
    creating: HashMap<String, (EnvKey, bool)>,
}

impl Registry {
    /// The environment for `key`, made by `make` the first time and returned unchanged after that.
    fn ensure_env(&mut self, key: &EnvKey, make: impl FnOnce() -> EnvInfo) -> EnvInfo {
        self.envs.entry(key.clone()).or_insert_with(make).clone()
    }

    /// Claims `tab_id` before its webview is built. A live or already-reserved id is `tab_exists`.
    fn reserve_tab(&mut self, tab_id: &str, key: EnvKey) -> Result<(), String> {
        if !self.envs.contains_key(&key) {
            return Err("unknown_env".to_string());
        }
        if self.tabs.contains_key(tab_id) || self.creating.contains_key(tab_id) {
            return Err("tab_exists".to_string());
        }
        self.creating.insert(tab_id.to_string(), (key, false));
        Ok(())
    }

    /// The webview is built: the reservation becomes a live tab. `unknown_tab` if the tab was closed
    /// (or the reservation released) meanwhile; the caller then closes the webview it built.
    fn finish_tab(&mut self, tab_id: &str) -> Result<(), String> {
        match self.creating.remove(tab_id) {
            Some((key, false)) => {
                self.tabs.insert(tab_id.to_string(), key);
                Ok(())
            }
            _ => Err("unknown_tab".to_string()),
        }
    }

    /// Drops a reservation whose build failed. A no-op for a live tab or an unknown id.
    fn release_tab(&mut self, tab_id: &str) {
        self.creating.remove(tab_id);
    }

    /// Removes a live tab, and cancels the creation of one that is still being built.
    fn remove_tab(&mut self, tab_id: &str) -> Option<EnvKey> {
        if let Some((_, cancelled)) = self.creating.get_mut(tab_id) {
            *cancelled = true;
        }
        self.tabs.remove(tab_id)
    }

    /// A copy of the environment registered for `key`, if any.
    fn env(&self, key: &EnvKey) -> Option<EnvInfo> {
        self.envs.get(key).cloned()
    }

    /// Registers `info` for `key`, replacing any previous environment.
    fn replace_env(&mut self, key: &EnvKey, info: EnvInfo) {
        self.envs.insert(key.clone(), info);
    }

    /// Forgets the environment `key`, so a dead one is never reused.
    fn remove_env(&mut self, key: &EnvKey) {
        self.envs.remove(key);
    }

    /// The ids of the tabs of `key` that are live or still being built. A rebuild closes both: it
    /// cancels the reservations, so a tab built against the old generation is closed by its creator.
    fn tabs_and_creating_of(&self, key: &EnvKey) -> Vec<String> {
        let mut ids = self.tabs_of(key);
        ids.extend(
            self.creating
                .iter()
                .filter(|(_, (k, _))| k == key)
                .map(|(id, _)| id.clone()),
        );
        ids
    }

    /// The ids of the live tabs of the environment `key`.
    fn tabs_of(&self, key: &EnvKey) -> Vec<String> {
        self.tabs
            .iter()
            .filter(|(_, k)| *k == key)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn env_of(&self, tab_id: &str) -> Option<&EnvKey> {
        self.tabs.get(tab_id)
    }

    #[cfg(test)]
    fn has_tab(&self, tab_id: &str) -> bool {
        self.env_of(tab_id).is_some()
    }
}

/// The managed state: the registry behind a lock. Never held across a tauri call that waits.
#[derive(Default)]
pub struct Browser {
    registry: Mutex<Registry>,
    /// Serialises agent-environment ensure calls for the whole build.
    agent_gate: Mutex<()>,
}

impl Browser {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// -------------------------------------------------------------------------------------------------
// Events
// -------------------------------------------------------------------------------------------------

/// What the handlers know about a tab between events.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabInfo {
    url: String,
    title: String,
    loading: bool,
}

impl TabInfo {
    fn new(url: &str) -> TabInfo {
        TabInfo {
            url: url.to_string(),
            title: String::new(),
            loading: false,
        }
    }

    fn to_state(&self, tab_id: &str, can_go_back: bool, can_go_forward: bool) -> TabState {
        TabState {
            tab_id: tab_id.to_string(),
            url: self.url.clone(),
            title: self.title.clone(),
            loading: self.loading,
            can_go_back,
            can_go_forward,
            secure: is_secure(&self.url),
        }
    }
}

type Shared = Arc<Mutex<TabInfo>>;

fn lock(info: &Shared) -> MutexGuard<'_, TabInfo> {
    info.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TabState {
    tab_id: String,
    url: String,
    title: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    secure: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvRebuilt {
    project: Option<String>,
    role: String,
    closed_tab_ids: Vec<String>,
}

// RED stub: skips a `None` project on purpose, so the test for `"project":null` fails.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct EnvRetired {
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    role: String,
}

/// The tab ids behind the labels of closed webviews, in order. Keepalive webviews and labels that
/// are not browser tabs are left out.
fn closed_tab_ids(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter_map(|label| tab_id_of_label(label))
        .filter(|id| !is_keepalive_tab_id(id))
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NewWindow {
    from_tab_id: String,
    url: String,
}

#[cfg(windows)]
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessFailed {
    tab_id: String,
    kind: &'static str,
}

fn publish(app: &AppHandle, tab_id: &str, info: &Shared, can_go_back: bool, can_go_forward: bool) {
    let state = lock(info).to_state(tab_id, can_go_back, can_go_forward);
    let _ = app.emit_to(MAIN_LABEL, TAB_STATE_EVENT, state);
}

/// Emits the tab's state. On Windows the history flags are read from `ICoreWebView2` on the main
/// thread (where this is called from, and where `with_webview` runs its closure); elsewhere they
/// are `false`: the non-Windows build is owner-only best effort.
fn emit_tab_state(app: &AppHandle, tab_id: &str, info: &Shared) {
    #[cfg(windows)]
    if let Some(webview) = app.get_webview(&label_for(tab_id)) {
        let (app, id, info) = (app.clone(), tab_id.to_string(), info.clone());
        let sent = webview.with_webview({
            let (app, id, info) = (app.clone(), id.clone(), info.clone());
            move |platform| {
                let (back, forward) = history_flags(&platform);
                publish(&app, &id, &info, back, forward);
            }
        });
        if sent.is_ok() {
            return;
        }
        publish(&app, &id, &info, false, false);
        return;
    }
    publish(app, tab_id, info, false, false);
}

#[cfg(windows)]
fn history_flags(platform: &tauri::webview::PlatformWebview) -> (bool, bool) {
    // SAFETY: plain COM call on the controller wry created, made on the thread that owns it.
    let Ok(core) = (unsafe { platform.controller().CoreWebView2() }) else {
        return (false, false);
    };
    core_history_flags(&core)
}

#[cfg(windows)]
fn core_history_flags(
    core: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
) -> (bool, bool) {
    // SAFETY: plain COM calls on a live `ICoreWebView2`, made on the thread that owns it.
    unsafe {
        let mut back = Default::default();
        let back_ok = core.CanGoBack(std::ptr::addr_of_mut!(back)).is_ok();
        let mut forward = Default::default();
        let forward_ok = core.CanGoForward(std::ptr::addr_of_mut!(forward)).is_ok();
        (back_ok && back.as_bool(), forward_ok && forward.as_bool())
    }
}

/// Reports `ProcessFailed` of the tab's WebView2 as `browser://process-failed`.
#[cfg(windows)]
fn watch_process_failures(webview: &tauri::Webview, app: &AppHandle, tab_id: &str) {
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND;

    let (app, id) = (app.clone(), tab_id.to_string());
    let _ = webview.with_webview(move |platform| {
        // SAFETY: COM calls on the controller wry created, on the thread that owns it. The handler
        // is reference-counted by COM and lives as long as the WebView2 does.
        unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            let handler =
                webview2_com::ProcessFailedEventHandler::create(Box::new(move |_sender, args| {
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    if let Some(args) = args {
                        let _ = args.ProcessFailedKind(&mut kind);
                    }
                    let payload = ProcessFailed {
                        tab_id: id.clone(),
                        kind: process_failed_kind(kind.0),
                    };
                    let _ = app.emit_to(MAIN_LABEL, PROCESS_FAILED_EVENT, payload);
                    Ok(())
                }));
            let mut token = 0i64;
            let _ = core.add_ProcessFailed(&handler, &mut token);
        }
    });
}

#[cfg(not(windows))]
fn watch_process_failures(_webview: &tauri::Webview, _app: &AppHandle, _tab_id: &str) {}

/// Re-publishes the tab state when its history changes without a page load (`history.pushState`,
/// hash navigations), so `canGoBack`/`canGoForward` do not go stale. Both handlers run on the
/// main thread, so the flags are read straight from the sender.
#[cfg(windows)]
fn watch_history(webview: &tauri::Webview, app: &AppHandle, tab_id: &str, info: &Shared) {
    let (app, id, info) = (app.clone(), tab_id.to_string(), info.clone());
    let _ = webview.with_webview(move |platform| {
        // SAFETY: COM calls on the controller wry created, on the thread that owns it. The handlers
        // are reference-counted by COM and live as long as the WebView2 does.
        unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            let (h_app, h_id, h_info) = (app.clone(), id.clone(), info.clone());
            let history =
                webview2_com::HistoryChangedEventHandler::create(Box::new(move |sender, _args| {
                    if let Some(core) = sender {
                        let (back, forward) = core_history_flags(&core);
                        publish(&h_app, &h_id, &h_info, back, forward);
                    }
                    Ok(())
                }));
            let mut token = 0i64;
            let _ = core.add_HistoryChanged(&history, &mut token);

            let source =
                webview2_com::SourceChangedEventHandler::create(Box::new(move |sender, _args| {
                    if let Some(core) = sender {
                        let (back, forward) = core_history_flags(&core);
                        publish(&app, &id, &info, back, forward);
                    }
                    Ok(())
                }));
            let mut token = 0i64;
            let _ = core.add_SourceChanged(&source, &mut token);
        }
    });
}

#[cfg(not(windows))]
fn watch_history(_webview: &tauri::Webview, _app: &AppHandle, _tab_id: &str, _info: &Shared) {}

/// Cancels iframe navigations that [`frame_navigation_allowed`] refuses, so a remote page cannot
/// frame the app's own origin (`on_navigation` only sees top-level navigations).
#[cfg(windows)]
fn guard_frames(webview: &tauri::Webview, app: &AppHandle) {
    let dev_url = app.config().build.dev_url.clone();
    let _ = webview.with_webview(move |platform| {
        // SAFETY: COM calls on the controller wry created, on the thread that owns it. The handler
        // is reference-counted by COM and lives as long as the WebView2 does.
        unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            let handler = webview2_com::NavigationStartingEventHandler::create(Box::new(
                move |_sender, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    // A null `PWSTR` without naming the type: `take` hands the null pointer over.
                    let mut raw = webview2_com::CoTaskMemPWSTR::default().take();
                    let allowed = args.Uri(&mut raw).is_ok()
                        && frame_navigation_allowed(
                            &webview2_com::take_pwstr(raw),
                            dev_url.as_ref(),
                        );
                    if !allowed {
                        let _ = args.SetCancel(true);
                    }
                    Ok(())
                },
            ));
            let mut token = 0i64;
            let _ = core.add_FrameNavigationStarting(&handler, &mut token);
        }
    });
}

#[cfg(not(windows))]
fn guard_frames(_webview: &tauri::Webview, _app: &AppHandle) {}

// -------------------------------------------------------------------------------------------------
// Webview glue
// -------------------------------------------------------------------------------------------------

/// A URL a tab may navigate to, parsed and checked against [`navigation_allowed`].
/// `agent` tabs are also kept off loopback hosts (see [`navigation_allowed_for`]).
fn checked_url(app: &AppHandle, raw: &str, agent: bool) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "invalid_url".to_string())?;
    if navigation_allowed_for(&url, app.config().build.dev_url.as_ref(), agent) {
        Ok(url)
    } else {
        Err("url_not_allowed".to_string())
    }
}

fn webview_of(app: &AppHandle, tab_id: &str) -> Result<tauri::Webview, String> {
    check_tab_id(tab_id)?;
    app.get_webview(&label_for(tab_id))
        .ok_or_else(|| "unknown_tab".to_string())
}

/// The environment for `key`. The owner one is created lazily; the agent one is only built by
/// [`ensure_agent_env`] (through `browser_env_ensure`), so here it is returned if registered and
/// `agent_env_missing` otherwise.
fn ensure_env(app: &AppHandle, key: &EnvKey) -> Result<EnvInfo, String> {
    let state = app.state::<Browser>();
    if key.role == Role::Agent {
        return state
            .registry()
            .env(key)
            .ok_or_else(|| "agent_env_missing".to_string());
    }
    let base = app.path().local_data_dir().map_err(|e| e.to_string())?;
    let mut registry = state.registry();
    Ok(registry.ensure_env(key, || EnvInfo {
        args: browser_args(&[]),
        data_dir: data_dir(&base, key),
        proxy_addr: None,
        cdp_port: None,
        generation: 0,
    }))
}

/// Builds (or reuses, or rebuilds) the agent environment of `key` for `proxy_addr`. Blocking-thread
/// only: it calls `add_child`, which must not run on the main thread (see the module docs). Calls
/// are serialised by `Browser::agent_gate`; the registry lock is never held across webview work.
fn ensure_agent_env(app: &AppHandle, key: &EnvKey, proxy_addr: &str) -> Result<EnvInfo, String> {
    let state = app.state::<Browser>();
    let _gate = state.agent_gate.lock().unwrap_or_else(|e| e.into_inner());

    let existing = state.registry().env(key);
    let decision = rebuild_decision(
        existing.as_ref().and_then(|e| e.proxy_addr.as_deref()),
        proxy_addr,
    );
    // Labels of the webviews actually closed by a rebuild, for the env-rebuilt event.
    let mut closed_labels: Vec<String> = Vec::new();
    let mut rebuilt = false;
    let generation = match (decision, &existing) {
        (RebuildDecision::Reuse, Some(env)) => return Ok(env.clone()),
        (RebuildDecision::Rebuild, Some(old)) => {
            rebuilt = true;
            // Close every webview of the environment, the keepalive included, before rebuilding.
            let old_keepalive = keepalive_label_for(&key.project, old.generation)?;
            // Tabs still being built are included: removing them cancels the reservation, so the
            // creator closes the webview it built against the old generation.
            let ids = state.registry().tabs_and_creating_of(key);
            for id in ids {
                let label = label_for(&id);
                if let Some(webview) = app.get_webview(&label) {
                    let _ = webview.close();
                    closed_labels.push(label);
                }
                state.registry().remove_tab(&id);
            }
            if let Some(webview) = app.get_webview(&old_keepalive) {
                let _ = webview.close();
            }
            // Always a fresh directory: WebView2 may refuse the old one with new arguments while
            // the old browser process still lingers.
            old.generation + 1
        }
        _ => 0,
    };

    let built = build_agent_env(app, key, proxy_addr, generation);
    match &built {
        Ok(env) => state.registry().replace_env(key, env.clone()),
        // The old webviews are already closed: drop the stale env so it is never reused.
        Err(_) if rebuilt => state.registry().remove_env(key),
        Err(_) => {}
    }
    if rebuilt {
        let _ = app.emit_to(
            MAIN_LABEL,
            ENV_REBUILT_EVENT,
            EnvRebuilt {
                project: Some(key.project.clone()),
                role: Role::Agent.as_str().to_string(),
                closed_tab_ids: closed_tab_ids(&closed_labels),
            },
        );
    }
    built
}

/// Picks the port, describes the environment of `generation` and starts its keepalive webview.
fn build_agent_env(
    app: &AppHandle,
    key: &EnvKey,
    proxy_addr: &str,
    generation: u32,
) -> Result<EnvInfo, String> {
    let base = app.path().local_data_dir().map_err(|e| e.to_string())?;
    let port = free_loopback_port()?;
    let extra = agent_args(proxy_addr, port);
    let refs: Vec<&str> = extra.iter().map(String::as_str).collect();
    let env = EnvInfo {
        args: browser_args(&refs),
        data_dir: data_dir(&base, key).with_file_name(generation_dir_name(generation)),
        proxy_addr: Some(proxy_addr.to_string()),
        cdp_port: Some(port),
        generation,
    };
    create_keepalive(app, &key.project, &env)?;
    Ok(env)
}

/// The hidden `about:blank` webview that keeps the agent environment's browser process (and so its
/// CDP port) alive. Built with a plain builder: no tab state, no handlers, no events.
fn create_keepalive(app: &AppHandle, project: &str, env: &EnvInfo) -> Result<(), String> {
    let window = app
        .get_window(MAIN_LABEL)
        .ok_or_else(|| "no_main_window".to_string())?;
    let blank = Url::parse("about:blank").map_err(|e| e.to_string())?;
    let builder = WebviewBuilder::new(
        keepalive_label_for(project, env.generation)?,
        WebviewUrl::External(blank),
    )
    .data_directory(env.data_dir.clone())
    .additional_browser_args(&env.args)
    .disable_drag_drop_handler();
    match window.add_child(
        builder,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(1.0, 1.0),
    ) {
        Ok(webview) => {
            let _ = webview.hide();
            Ok(())
        }
        Err(e) => {
            let message = e.to_string();
            if looks_like_runtime_missing(&message) {
                let _ = app.emit_to(MAIN_LABEL, RUNTIME_MISSING_EVENT, serde_json::json!({}));
            }
            Err(message)
        }
    }
}

fn build_webview(
    app: &AppHandle,
    tab_id: &str,
    url: Url,
    env: &EnvInfo,
    info: &Shared,
) -> WebviewBuilder<tauri::Wry> {
    let dev_url = app.config().build.dev_url.clone();

    let (nav_app, nav_id, nav_info) = (app.clone(), tab_id.to_string(), info.clone());
    let (win_app, win_id) = (app.clone(), tab_id.to_string());
    let (load_app, load_id, load_info) = (app.clone(), tab_id.to_string(), info.clone());
    let (title_app, title_id, title_info) = (app.clone(), tab_id.to_string(), info.clone());
    let win_dev_url = dev_url.clone();
    // Only the agent environment has a proxy.
    let agent = env.proxy_addr.is_some();

    WebviewBuilder::new(label_for(tab_id), WebviewUrl::External(url))
        .data_directory(env.data_dir.clone())
        .additional_browser_args(&env.args)
        // The page handles its own drops; tauri's handler would turn a file dropped on a tab into
        // the app's `files://dropped`.
        .disable_drag_drop_handler()
        .on_navigation(move |next| {
            if !navigation_allowed_for(next, dev_url.as_ref(), agent) {
                return false;
            }
            {
                let mut tab = lock(&nav_info);
                tab.url = next.to_string();
                tab.loading = true;
            }
            emit_tab_state(&nav_app, &nav_id, &nav_info);
            true
        })
        .on_new_window(move |requested, _features| {
            if navigation_allowed_for(&requested, win_dev_url.as_ref(), agent) {
                let payload = NewWindow {
                    from_tab_id: win_id.clone(),
                    url: requested.to_string(),
                };
                let _ = win_app.emit_to(MAIN_LABEL, NEW_WINDOW_EVENT, payload);
            }
            NewWindowResponse::Deny
        })
        .on_page_load(move |_webview, payload| {
            {
                let mut tab = lock(&load_info);
                tab.url = payload.url().to_string();
                tab.loading = matches!(payload.event(), PageLoadEvent::Started);
            }
            emit_tab_state(&load_app, &load_id, &load_info);
        })
        .on_document_title_changed(move |_webview, title| {
            lock(&title_info).title = title;
            emit_tab_state(&title_app, &title_id, &title_info);
        })
}

/// Runs on a blocking thread, never the main one: see the module docs.
fn create_tab(
    app: &AppHandle,
    key: EnvKey,
    tab_id: String,
    url: Url,
    bounds: Bounds,
) -> Result<(), String> {
    let state = app.state::<Browser>();
    // For an agent tab the env lookup and the reservation happen under `agent_gate`, so they are
    // atomic with a rebuild: a rebuild either finished before (new generation seen) or starts after
    // (and cancels this reservation). `ensure_agent_env` never calls this function, so the gate is
    // never taken twice in one task. The gate is released before the webview is built.
    let gate = (key.role == Role::Agent)
        .then(|| state.agent_gate.lock().unwrap_or_else(|e| e.into_inner()));
    let env = ensure_env(app, &key)?;
    // Reserved before `add_child`, so a close that arrives while the webview is being built is seen.
    state.registry().reserve_tab(&tab_id, key)?;
    drop(gate);
    let result = create_reserved(app, &tab_id, url, bounds, &env);
    if result.is_err() {
        // A no-op once `finish_tab` ran; frees the id when the build failed before it.
        app.state::<Browser>().registry().release_tab(&tab_id);
    }
    result
}

/// The part of [`create_tab`] that runs with the id reserved. Any error after `add_child` closes
/// the webview, so none is left behind without a registry entry.
fn create_reserved(
    app: &AppHandle,
    tab_id: &str,
    url: Url,
    bounds: Bounds,
    env: &EnvInfo,
) -> Result<(), String> {
    let window = app
        .get_window(MAIN_LABEL)
        .ok_or_else(|| "no_main_window".to_string())?;

    let info: Shared = Arc::new(Mutex::new(TabInfo::new(url.as_str())));
    let builder = build_webview(app, tab_id, url, env, &info);
    let webview = match window.add_child(
        builder,
        LogicalPosition::new(bounds.x, bounds.y),
        LogicalSize::new(bounds.width, bounds.height),
    ) {
        Ok(webview) => webview,
        Err(e) => {
            let message = e.to_string();
            if looks_like_runtime_missing(&message) {
                let _ = app.emit_to(MAIN_LABEL, RUNTIME_MISSING_EVENT, serde_json::json!({}));
            }
            return Err(message);
        }
    };
    // Born hidden: `browser_tabs_show` decides what is on screen.
    let _ = webview.hide();
    watch_process_failures(&webview, app, tab_id);
    watch_history(&webview, app, tab_id, &info);
    guard_frames(&webview, app);
    // Bind the result so the registry guard is dropped before the webview is closed.
    let finished = app.state::<Browser>().registry().finish_tab(tab_id);
    if finished.is_err() {
        // Closed while it was being built: the close found no webview, so this one is ours to close.
        let _ = webview.close();
    }
    finished
}

// -------------------------------------------------------------------------------------------------
// Commands
// -------------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct EnsureReply {
    /// `None` for the owner environment: it has no CDP port.
    cdp_http: Option<String>,
}

/// Accepts only `host:port` for the agent proxy, since the value is spliced into
/// `--proxy-server=http://{proxy_addr}`: a hostname/IPv4 of `[A-Za-z0-9.-]` or a bracketed IPv6
/// literal, then a numeric, nonzero `u16` port. Anything else (spaces, commas, semicolons, slashes,
/// `=`, `@`, quotes, a scheme) could smuggle extra browser flags or redirect the proxy.
fn check_proxy_addr(addr: &str) -> Result<(), String> {
    let bad = || "agent_proxy_invalid".to_string();
    let (host, port) = addr.rsplit_once(':').ok_or_else(bad)?;
    let host_ok = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    } else {
        !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    };
    let port_ok = !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != 0);
    if host_ok && port_ok {
        Ok(())
    } else {
        Err(bad())
    }
}

/// `async` so tauri runs it off the main thread; the agent build (which calls `add_child`) goes to
/// `spawn_blocking`. `proxy_addr` is required for the agent environment and refused for the owner.
#[tauri::command]
pub async fn browser_env_ensure(
    app: AppHandle,
    project: String,
    role: String,
    proxy_addr: Option<String>,
) -> Result<EnsureReply, String> {
    let key = EnvKey::new(&project, &role)?;
    match key.role {
        Role::Owner => {
            if proxy_addr.is_some() {
                return Err("owner_proxy_unexpected".to_string());
            }
            ensure_env(&app, &key)?;
            Ok(EnsureReply { cdp_http: None })
        }
        Role::Agent => {
            let proxy = proxy_addr
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .ok_or_else(|| "agent_proxy_required".to_string())?;
            check_proxy_addr(&proxy)?;
            keepalive_tab_id(&project)?;
            let env =
                tauri::async_runtime::spawn_blocking(move || ensure_agent_env(&app, &key, &proxy))
                    .await
                    .map_err(|e| e.to_string())??;
            let port = env
                .cdp_port
                .ok_or_else(|| "agent_env_missing".to_string())?;
            Ok(EnsureReply {
                cdp_http: Some(format!("http://127.0.0.1:{port}")),
            })
        }
    }
}

/// `async` so tauri runs it off the main thread, and the build itself on `spawn_blocking`: see the
/// module docs on `add_child`.
#[tauri::command]
pub async fn browser_tab_create(
    app: AppHandle,
    project: String,
    role: String,
    tab_id: String,
    url: String,
    bounds: Bounds,
) -> Result<(), String> {
    let key = EnvKey::new(&project, &role)?;
    check_tab_id(&tab_id)?;
    bounds.validate()?;
    let url = checked_url(&app, &url, key.role == Role::Agent)?;
    tauri::async_runtime::spawn_blocking(move || create_tab(&app, key, tab_id, url, bounds))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn browser_tab_navigate(app: AppHandle, tab_id: String, url: String) -> Result<(), String> {
    let webview = webview_of(&app, &tab_id)?;
    let agent = app
        .state::<Browser>()
        .registry()
        .env_of(&tab_id)
        .is_some_and(|k| k.role == Role::Agent);
    let url = checked_url(&app, &url, agent)?;
    webview.navigate(url).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy)]
enum History {
    Back,
    Forward,
    Stop,
}

fn history(app: &AppHandle, tab_id: &str, action: History) -> Result<(), String> {
    let webview = webview_of(app, tab_id)?;
    #[cfg(windows)]
    {
        webview
            .with_webview(move |platform| {
                // SAFETY: COM calls on the controller wry created, on the thread that owns it.
                unsafe {
                    if let Ok(core) = platform.controller().CoreWebView2() {
                        let _ = match action {
                            History::Back => core.GoBack(),
                            History::Forward => core.GoForward(),
                            History::Stop => core.Stop(),
                        };
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let script = match action {
            History::Back => "history.back()",
            History::Forward => "history.forward()",
            History::Stop => "window.stop()",
        };
        webview.eval(script).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn browser_tab_back(app: AppHandle, tab_id: String) -> Result<(), String> {
    history(&app, &tab_id, History::Back)
}

#[tauri::command]
pub fn browser_tab_forward(app: AppHandle, tab_id: String) -> Result<(), String> {
    history(&app, &tab_id, History::Forward)
}

#[tauri::command]
pub fn browser_tab_stop(app: AppHandle, tab_id: String) -> Result<(), String> {
    history(&app, &tab_id, History::Stop)
}

#[tauri::command]
pub fn browser_tab_reload(app: AppHandle, tab_id: String) -> Result<(), String> {
    webview_of(&app, &tab_id)?
        .reload()
        .map_err(|e| e.to_string())
}

/// Shows exactly `tab_id` at `bounds` and hides every other browser webview; `None` hides all.
#[tauri::command]
pub fn browser_tabs_show(
    app: AppHandle,
    tab_id: Option<String>,
    bounds: Option<Bounds>,
) -> Result<(), String> {
    let rect = match &tab_id {
        Some(id) => {
            check_tab_id(id)?;
            let bounds = bounds.ok_or_else(|| "bounds_required".to_string())?;
            bounds.validate()?;
            Some(bounds.rect())
        }
        None => None,
    };
    let window = app
        .get_window(MAIN_LABEL)
        .ok_or_else(|| "no_main_window".to_string())?;
    // Every browser webview of the window, not only the registered ones.
    let tabs: Vec<String> = window
        .webviews()
        .iter()
        .filter_map(|w| tab_id_of_label(w.label()))
        .filter(|id| !is_keepalive_tab_id(id))
        .map(str::to_string)
        .collect();

    // Apply the plan even when it carries an error: an unknown target must still hide the others.
    let plan = plan_show(&tabs, tab_id.as_deref());
    let mut first_error = None;
    for (id, visible) in plan.visibility {
        let Some(webview) = app.get_webview(&label_for(&id)) else {
            continue;
        };
        let done = match (visible, rect) {
            (true, Some(rect)) => webview.set_bounds(rect).and_then(|_| webview.show()),
            _ => webview.hide(),
        };
        if let Err(e) = done {
            if first_error.is_none() {
                first_error = Some(e.to_string());
            }
        }
    }
    if let Some(error) = plan.error {
        return Err(error);
    }
    first_error.map_or(Ok(()), Err)
}

#[tauri::command]
pub fn browser_tab_close(app: AppHandle, tab_id: String) -> Result<(), String> {
    check_tab_id(&tab_id)?;
    app.state::<Browser>().registry().remove_tab(&tab_id);
    match app.get_webview(&label_for(&tab_id)) {
        Some(webview) => webview.close().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn envkey_none_scope_is_distinct_from_every_project() {
        assert_ne!(
            EnvKey::none(Role::Owner),
            EnvKey::new("p", "owner").unwrap()
        );
        assert_ne!(
            EnvKey::none(Role::Agent),
            EnvKey::new("none", "agent").unwrap_or_else(|_| EnvKey::none(Role::Owner))
        );
        assert_eq!(EnvKey::scoped(None, Role::Owner), EnvKey::none(Role::Owner));
        assert_eq!(EnvKey::scoped(None, Role::Agent), EnvKey::none(Role::Agent));
        assert_ne!(EnvKey::none(Role::Owner), EnvKey::none(Role::Agent));
        assert_eq!(
            EnvKey::scoped(Some("p"), Role::Owner),
            EnvKey::new("p", "owner").unwrap()
        );
    }

    #[test]
    fn envkey_refuses_a_project_named_none() {
        assert_eq!(
            EnvKey::new("none", "owner"),
            Err("invalid_project".to_string())
        );
        assert_eq!(
            EnvKey::new("none", "agent"),
            Err("invalid_project".to_string())
        );
    }

    #[test]
    fn data_dir_of_the_no_project_scope_is_browser_none_role() {
        let base = Path::new("base");
        let root = base.join("nucleos").join("browser");
        assert_eq!(
            data_dir(base, &EnvKey::none(Role::Owner)),
            root.join("none").join("owner")
        );
        assert_eq!(
            data_dir(base, &EnvKey::none(Role::Agent)),
            root.join("none").join("agent")
        );
        assert_eq!(
            data_dir(base, &EnvKey::new("proj", "owner").unwrap()),
            root.join("proj").join("owner")
        );
    }

    #[test]
    fn registry_keeps_no_project_and_project_environments_apart() {
        let mut reg = Registry::default();
        let none = EnvKey::none(Role::Agent);
        let proj = EnvKey::new("proj", "agent").unwrap();
        reg.ensure_env(&none, || env("none-args"));
        reg.ensure_env(&proj, || env("proj-args"));
        assert_eq!(reg.env(&none).unwrap().args, "none-args");
        assert_eq!(reg.env(&proj).unwrap().args, "proj-args");

        reg.reserve_tab("t-none", none.clone()).unwrap();
        reg.finish_tab("t-none").unwrap();
        reg.reserve_tab("t-proj", proj.clone()).unwrap();
        reg.finish_tab("t-proj").unwrap();
        assert_eq!(reg.tabs_of(&none), vec!["t-none".to_string()]);
        assert_eq!(reg.tabs_of(&proj), vec!["t-proj".to_string()]);

        assert_eq!(reg.remove_tab("t-none"), Some(none.clone()));
        assert!(reg.tabs_of(&none).is_empty());
        assert_eq!(reg.tabs_of(&proj), vec!["t-proj".to_string()]);
    }

    #[test]
    fn the_no_project_agent_env_retires_only_when_its_last_tab_is_gone() {
        let none = EnvKey::none(Role::Agent);
        let mut reg = Registry::default();
        reg.ensure_env(&none, || env("a"));
        assert!(should_retire_none_agent(&reg, &none));

        reg.reserve_tab("t1", none.clone()).unwrap();
        assert!(!should_retire_none_agent(&reg, &none), "creating");
        reg.finish_tab("t1").unwrap();
        assert!(!should_retire_none_agent(&reg, &none), "live");
        reg.remove_tab("t1");
        assert!(should_retire_none_agent(&reg, &none));

        let owner = EnvKey::none(Role::Owner);
        let proj = EnvKey::new("proj", "agent").unwrap();
        reg.ensure_env(&owner, || env("o"));
        reg.ensure_env(&proj, || env("p"));
        assert!(!should_retire_none_agent(&reg, &owner));
        assert!(!should_retire_none_agent(&reg, &proj));
    }

    #[test]
    fn stale_none_agent_dirs_are_everything_but_the_live_generation() {
        // generation_dir_name: generation 0 -> `agent`, N -> `agent-N`. `owner` is never touched.
        let names = labels(&["agent", "agent-1", "agent-2", "owner"]);
        assert_eq!(
            stale_none_agent_dirs(&names, Some(1)),
            labels(&["agent", "agent-2"])
        );
        assert_eq!(
            stale_none_agent_dirs(&names, Some(0)),
            labels(&["agent-1", "agent-2"])
        );
        assert_eq!(
            stale_none_agent_dirs(&names, None),
            labels(&["agent", "agent-1", "agent-2"])
        );
    }

    #[test]
    fn env_rebuilt_payload_has_a_null_project_for_the_no_project_scope() {
        let rebuilt = EnvRebuilt {
            project: None,
            role: "agent".to_string(),
            closed_tab_ids: vec!["a".to_string()],
        };
        let json = serde_json::to_string(&rebuilt).unwrap();
        assert!(json.contains("\"project\":null"), "{json}");
        assert!(json.contains("\"closedTabIds\":[\"a\"]"), "{json}");

        let retired = EnvRetired {
            project: None,
            role: "agent".to_string(),
        };
        let json = serde_json::to_string(&retired).unwrap();
        assert!(json.contains("\"project\":null"), "{json}");
        assert!(json.contains("\"role\":\"agent\""), "{json}");
    }

    #[test]
    fn check_proxy_addr_accepts_host_port_only() {
        for good in [
            "127.0.0.1:8080",
            "localhost:1",
            "my-host.local:65535",
            "[::1]:3128",
        ] {
            assert!(check_proxy_addr(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ":80",
            "127.0.0.1",
            "127.0.0.1:",
            "127.0.0.1:0",
            "127.0.0.1:65536",
            "127.0.0.1:+80",
            "127.0.0.1:80 --no-sandbox",
            "a,b:80",
            "a;b:80",
            "a/b:80",
            "http://127.0.0.1:80",
            "a=b:80",
            "u@127.0.0.1:80",
            "\"x\":80",
            "::1:80",
            "[]:80",
            "[::1:80",
            "[g::1]:80",
        ] {
            assert!(check_proxy_addr(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn closed_tab_ids_maps_labels_in_order() {
        let got = closed_tab_ids(&labels(&["browser-b", "browser-a", "browser-c"]));
        assert_eq!(got, vec!["b", "a", "c"]);
    }

    #[test]
    fn closed_tab_ids_excludes_keepalive_labels() {
        let got = closed_tab_ids(&labels(&[
            "browser-a",
            "browser-agent-keepalive-p",
            "browser-b",
        ]));
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn closed_tab_ids_skips_labels_without_prefix() {
        let got = closed_tab_ids(&labels(&["main", "notch", "browser-", "browser-a"]));
        assert_eq!(got, vec!["a"]);
    }

    #[test]
    fn closed_tab_ids_of_nothing_is_empty() {
        assert!(closed_tab_ids(&[]).is_empty());
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn key(project: &str, role: Role) -> EnvKey {
        EnvKey {
            project: project.to_string(),
            role,
        }
    }

    fn env(args: &str) -> EnvInfo {
        EnvInfo {
            args: args.to_string(),
            data_dir: PathBuf::from("d"),
            proxy_addr: None,
            cdp_port: None,
            generation: 0,
        }
    }

    #[test]
    fn owner_and_agent_envs_of_one_project_are_distinct() {
        let owner = EnvKey::new("p", "owner").unwrap();
        let agent = EnvKey::new("p", "agent").unwrap();
        assert_ne!(owner, agent);
        let base = Path::new("base");
        assert_ne!(data_dir(base, &owner), data_dir(base, &agent));
        assert!(data_dir(base, &agent).ends_with("agent"));

        let mut registry = Registry::default();
        let a = registry.ensure_env(&owner, || env("owner-args"));
        let b = registry.ensure_env(&agent, || env("agent-args"));
        assert_eq!(a.args, "owner-args");
        assert_eq!(b.args, "agent-args");
        assert_eq!(
            registry.ensure_env(&owner, || env("other")).args,
            "owner-args"
        );
    }

    #[test]
    fn registry_env_replace_and_tabs_of() {
        let owner = key("p", Role::Owner);
        let agent = key("p", Role::Agent);
        let mut registry = Registry::default();
        assert_eq!(registry.env(&agent), None);
        registry.replace_env(&agent, env("one"));
        assert_eq!(registry.env(&agent), Some(env("one")));
        registry.replace_env(&agent, env("two"));
        assert_eq!(registry.env(&agent).unwrap().args, "two");
        assert_eq!(registry.env(&owner), None);

        registry.replace_env(&owner, env("o"));
        registry.reserve_tab("t1", agent.clone()).unwrap();
        registry.finish_tab("t1").unwrap();
        registry.reserve_tab("t2", owner.clone()).unwrap();
        registry.finish_tab("t2").unwrap();
        assert_eq!(registry.tabs_of(&agent), vec!["t1".to_string()]);
        assert_eq!(registry.tabs_of(&owner), vec!["t2".to_string()]);
        registry.remove_tab("t1");
        assert!(registry.tabs_of(&agent).is_empty());
    }

    #[test]
    fn keepalive_tab_ids_are_refused_as_tab_ids() {
        assert_eq!(
            check_tab_id("agent-keepalive-p"),
            Err("invalid_tab_id".to_string())
        );
    }

    #[test]
    fn agent_generation_dirs_are_siblings_of_the_base_dir() {
        let agent = EnvKey::new("p", "agent").unwrap();
        let base = Path::new("base");
        let g = data_dir(base, &agent).with_file_name(generation_dir_name(2));
        assert!(g.ends_with("agent-2"));
        assert_eq!(g.parent(), data_dir(base, &agent).parent());
    }

    #[test]
    fn only_https_is_secure() {
        assert!(is_secure("https://example.com/a"));
        assert!(!is_secure("http://example.com/"));
        assert!(!is_secure("about:blank"));
        assert!(!is_secure("not a url"));
        assert!(!is_secure(""));
    }

    #[test]
    fn labels_round_trip_and_foreign_labels_are_not_tabs() {
        assert_eq!(label_for("abc-1"), "browser-abc-1");
        assert_eq!(tab_id_of_label("browser-abc-1"), Some("abc-1"));
        assert_eq!(tab_id_of_label(&label_for("x_y")), Some("x_y"));
        assert_eq!(tab_id_of_label("main"), None);
        assert_eq!(tab_id_of_label("notch"), None);
        assert_eq!(tab_id_of_label("browser-"), None);
    }

    #[test]
    fn tab_ids_keep_to_the_label_alphabet() {
        assert!(check_tab_id("3f2a9c1e-77aa-4b8d-9d51-0a1b2c3d4e5f").is_ok());
        assert!(check_tab_id("").is_err());
        assert!(check_tab_id("a b").is_err());
        assert!(check_tab_id("a/b").is_err());
        assert!(check_tab_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn roles_parse_and_anything_else_is_refused() {
        assert_eq!(Role::parse("owner"), Ok(Role::Owner));
        assert_eq!(Role::parse("agent"), Ok(Role::Agent));
        assert_eq!(Role::parse("Owner"), Err("invalid_role".to_string()));
        assert_eq!(Role::parse(""), Err("invalid_role".to_string()));
    }

    #[test]
    fn an_env_key_needs_a_valid_role_and_project() {
        assert_eq!(
            EnvKey::new("proj-1", "owner"),
            Ok(key("proj-1", Role::Owner))
        );
        assert_eq!(
            EnvKey::new("proj-1", "root"),
            Err("invalid_role".to_string())
        );
        assert_eq!(
            EnvKey::new("../x", "owner"),
            Err("invalid_project".to_string())
        );
    }

    #[test]
    fn project_ids_that_could_leave_their_directory_are_refused() {
        for bad in [
            "", "..", "a/b", "a\\b", "..\\x", "a..b", ".hidden", "trail.", "c:", "a*b", "a\0b",
            "con", "NUL", "com1", "lpt9.txt", " x", "x ",
        ] {
            assert!(check_project(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(check_project(&"a".repeat(129)).is_err());
        for good in [
            "proj",
            "my-project_2",
            "3f2a9c1e-77aa-4b8d-9d51-0a1b2c3d4e5f",
            "a.b",
            "console",
        ] {
            assert!(check_project(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn the_data_dir_is_base_nucleos_browser_project_role() {
        let base = Path::new("base");
        let expected: PathBuf = ["base", "nucleos", "browser", "proj", "owner"]
            .iter()
            .collect();
        assert_eq!(data_dir(base, &key("proj", Role::Owner)), expected);
        assert_ne!(
            data_dir(base, &key("proj", Role::Owner)),
            data_dir(base, &key("proj", Role::Agent))
        );
        assert_ne!(
            data_dir(base, &key("a", Role::Owner)),
            data_dir(base, &key("b", Role::Owner))
        );
    }

    #[test]
    fn browser_args_restore_the_defaults_and_append_extras_in_order() {
        assert_eq!(
            browser_args(&[]),
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection"
        );
        assert_eq!(
            browser_args(&["--a=1", "--b"]),
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --a=1 --b"
        );
        assert_eq!(browser_args(&["--a=1"]), browser_args(&["--a=1"]));
    }

    #[test]
    fn bounds_must_be_finite_with_a_positive_size() {
        let b = |x, y, width, height| Bounds {
            x,
            y,
            width,
            height,
        };
        assert!(b(0.0, 0.0, 10.0, 10.0).validate().is_ok());
        assert!(b(-5.0, -5.0, 10.0, 10.0).validate().is_ok());
        assert!(b(0.0, 0.0, 0.0, 10.0).validate().is_err());
        assert!(b(0.0, 0.0, 10.0, 0.0).validate().is_err());
        assert!(b(0.0, 0.0, -1.0, 10.0).validate().is_err());
        assert!(b(0.0, 0.0, 10.0, -1.0).validate().is_err());
        assert!(b(f64::NAN, 0.0, 10.0, 10.0).validate().is_err());
        assert!(b(0.0, f64::INFINITY, 10.0, 10.0).validate().is_err());
        assert!(b(0.0, 0.0, f64::NAN, 10.0).validate().is_err());
        assert!(b(0.0, 0.0, 10.0, f64::INFINITY).validate().is_err());
    }

    #[test]
    fn bounds_deserialise_from_the_json_the_shell_sends() {
        let b: Bounds =
            serde_json::from_str(r#"{"x":1.5,"y":2,"width":300,"height":200}"#).unwrap();
        assert_eq!(
            b,
            Bounds {
                x: 1.5,
                y: 2.0,
                width: 300.0,
                height: 200.0
            }
        );
    }

    #[test]
    fn navigation_allows_the_web_and_about_blank_only() {
        assert!(navigation_allowed(&url("https://example.com/"), None));
        assert!(navigation_allowed(&url("http://example.com:8080/x"), None));
        assert!(navigation_allowed(&url("about:blank"), None));
        assert!(navigation_allowed(&url("about:blank#popup"), None));
        assert!(!navigation_allowed(&url("about:srcdoc"), None));
        assert!(!navigation_allowed(
            &url("file:///c:/windows/win.ini"),
            None
        ));
        assert!(!navigation_allowed(&url("tauri://localhost/"), None));
        assert!(!navigation_allowed(&url("javascript:alert(1)"), None));
        assert!(!navigation_allowed(&url("data:text/html,hi"), None));
        assert!(!navigation_allowed(&url("ipc://localhost/x"), None));
    }

    #[test]
    fn frames_may_not_load_the_app_but_keep_everyday_content() {
        let dev = url("http://localhost:1420/");
        assert!(frame_navigation_allowed("https://example.com/embed", None));
        assert!(frame_navigation_allowed("about:blank", None));
        assert!(frame_navigation_allowed("about:srcdoc", None));
        assert!(frame_navigation_allowed(
            "blob:https://example.com/abc",
            None
        ));
        assert!(frame_navigation_allowed("data:text/html,hi", None));
        assert!(!frame_navigation_allowed("not a url", None));
        assert!(!frame_navigation_allowed("", None));
        assert!(!frame_navigation_allowed("http://tauri.localhost/", None));
        assert!(!frame_navigation_allowed("https://ipc.localhost/x", None));
        assert!(!frame_navigation_allowed(
            "file:///c:/windows/win.ini",
            None
        ));
        assert!(!frame_navigation_allowed(
            "http://localhost:1420/",
            Some(&dev)
        ));
        assert!(frame_navigation_allowed(
            "http://localhost:3000/",
            Some(&dev)
        ));
    }

    #[test]
    fn agent_tabs_never_reach_loopback_but_user_tabs_may() {
        for raw in [
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/",
            "http://2130706433/",
            "http://[::1]:3000/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            "http://localhost:3000/",
            "http://LOCALHOST./",
            "http://foo.localhost/",
        ] {
            assert!(!navigation_allowed_for(&url(raw), None, true), "{raw}");
        }
        for raw in [
            "http://127.0.0.1:8080/",
            "http://[::1]:3000/",
            "http://localhost:3000/",
        ] {
            assert!(navigation_allowed_for(&url(raw), None, false), "{raw}");
        }
        for raw in ["https://example.com/", "http://8.8.8.8/", "about:blank"] {
            assert!(navigation_allowed_for(&url(raw), None, true), "{raw}");
        }
    }

    #[test]
    fn navigation_never_reaches_the_apps_own_origins() {
        assert!(!navigation_allowed(&url("http://tauri.localhost/"), None));
        assert!(!navigation_allowed(&url("https://tauri.localhost/x"), None));
        assert!(!navigation_allowed(&url("http://ipc.localhost/"), None));
        assert!(!navigation_allowed(&url("http://tauri.localhost./"), None));
        // A dev server of the owner's is fine; the app's own devUrl is not.
        let dev = url("http://localhost:1420/");
        assert!(navigation_allowed(
            &url("http://localhost:3000/"),
            Some(&dev)
        ));
        assert!(navigation_allowed(&url("http://localhost:3000/"), None));
        assert!(!navigation_allowed(
            &url("http://localhost:1420/x?y=1"),
            Some(&dev)
        ));
        assert!(navigation_allowed(
            &url("https://localhost:1420/"),
            Some(&dev)
        ));
    }

    #[test]
    fn plan_show_shows_exactly_the_target() {
        let tabs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            plan_show(&tabs, Some("b")),
            ShowPlan {
                visibility: vec![
                    ("a".to_string(), false),
                    ("b".to_string(), true),
                    ("c".to_string(), false)
                ],
                error: None,
            }
        );
        let none = plan_show(&tabs, None);
        assert!(none.visibility.iter().all(|(_, v)| !v));
        assert_eq!(none.error, None);
        assert_eq!(
            plan_show(&[], None),
            ShowPlan {
                visibility: vec![],
                error: None
            }
        );
    }

    #[test]
    fn plan_show_of_an_unknown_target_still_hides_every_tab() {
        let tabs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            plan_show(&tabs, Some("z")),
            ShowPlan {
                visibility: vec![("a".to_string(), false), ("b".to_string(), false)],
                error: Some("unknown_tab".to_string()),
            }
        );
    }

    #[test]
    fn process_failures_are_named() {
        assert_eq!(process_failed_kind(0), "browser_process_exited");
        assert_eq!(process_failed_kind(1), "render_process_exited");
        assert_eq!(process_failed_kind(2), "render_process_unresponsive");
        assert_eq!(process_failed_kind(6), "gpu_process_exited");
        assert_eq!(process_failed_kind(99), "unknown_process_exited");
    }

    #[test]
    fn a_missing_runtime_is_recognised_from_the_error_text() {
        assert!(looks_like_runtime_missing(
            "WebView2Error(HRESULT(0x80070002))"
        ));
        assert!(looks_like_runtime_missing("WebView2 Runtime not found"));
        assert!(!looks_like_runtime_missing(
            "a webview with label `x` already exists"
        ));
        assert!(!looks_like_runtime_missing(""));
    }

    #[test]
    fn registry_fixes_an_envs_args_at_first_creation() {
        let mut reg = Registry::default();
        let k = key("p", Role::Owner);
        let first = reg.ensure_env(&k, || env("--first"));
        let second = reg.ensure_env(&k, || env("--second"));
        assert_eq!(first, env("--first"));
        assert_eq!(second, env("--first"));
        let other = reg.ensure_env(&key("p", Role::Agent), || env("--other"));
        assert_eq!(other, env("--other"));
    }

    #[test]
    fn registry_tracks_tabs_against_their_env() {
        let mut reg = Registry::default();
        let k = key("p", Role::Owner);
        assert_eq!(
            reg.reserve_tab("t1", k.clone()),
            Err("unknown_env".to_string())
        );
        reg.ensure_env(&k, || env("--x"));
        assert_eq!(reg.reserve_tab("t1", k.clone()), Ok(()));
        assert_eq!(reg.finish_tab("t1"), Ok(()));
        assert_eq!(
            reg.reserve_tab("t1", k.clone()),
            Err("tab_exists".to_string())
        );
        assert!(reg.has_tab("t1"));
        assert_eq!(reg.env_of("t1"), Some(&k));
        assert!(!reg.has_tab("t2"));
        assert_eq!(reg.remove_tab("t1"), Some(k.clone()));
        assert_eq!(reg.remove_tab("t1"), None);
        assert!(!reg.has_tab("t1"));
        // The env outlives its tabs, so its arguments stay fixed for the next tab.
        assert_eq!(reg.ensure_env(&k, || env("--y")), env("--x"));
        assert_eq!(reg.reserve_tab("t1", k), Ok(()));
    }

    fn registry_with_env() -> (Registry, EnvKey) {
        let mut reg = Registry::default();
        let k = key("p", Role::Owner);
        reg.ensure_env(&k, || env("--x"));
        (reg, k)
    }

    #[test]
    fn remove_env_forgets_the_env_and_blocks_new_reservations() {
        let (mut reg, k) = registry_with_env();
        reg.remove_env(&k);
        assert_eq!(reg.env(&k), None);
        assert_eq!(reg.reserve_tab("t1", k), Err("unknown_env".to_string()));
    }

    #[test]
    fn a_rebuild_cancels_tabs_still_being_built() {
        let (mut reg, k) = registry_with_env();
        reg.reserve_tab("live", k.clone()).unwrap();
        reg.finish_tab("live").unwrap();
        reg.reserve_tab("building", k.clone()).unwrap();
        let mut ids = reg.tabs_and_creating_of(&k);
        ids.sort();
        assert_eq!(ids, vec!["building", "live"]);
        for id in ids {
            reg.remove_tab(&id);
        }
        // The build that was in flight can no longer become a live tab.
        assert_eq!(reg.finish_tab("building"), Err("unknown_tab".to_string()));
        assert!(!reg.has_tab("live"));
    }

    #[test]
    fn keepalive_labels_of_any_generation_are_not_tab_ids() {
        let label = keepalive_label_for("p", 3).unwrap();
        assert_eq!(
            closed_tab_ids(std::slice::from_ref(&label)),
            Vec::<String>::new()
        );
        let id = tab_id_of_label(&label).unwrap();
        assert_eq!(check_tab_id(id), Err("invalid_tab_id".to_string()));
    }

    #[test]
    fn a_reserved_tab_is_not_live_until_finished() {
        let (mut reg, k) = registry_with_env();
        assert_eq!(reg.reserve_tab("t1", k.clone()), Ok(()));
        assert!(!reg.has_tab("t1"));
        assert_eq!(reg.finish_tab("t1"), Ok(()));
        assert!(reg.has_tab("t1"));
        assert_eq!(reg.env_of("t1"), Some(&k));
    }

    #[test]
    fn a_duplicate_reservation_is_tab_exists() {
        let (mut reg, k) = registry_with_env();
        assert_eq!(reg.reserve_tab("t1", k.clone()), Ok(()));
        assert_eq!(
            reg.reserve_tab("t1", k.clone()),
            Err("tab_exists".to_string())
        );
        // A different id is unaffected.
        assert_eq!(reg.reserve_tab("t2", k), Ok(()));
    }

    #[test]
    fn closing_a_tab_while_it_is_created_cancels_the_creation() {
        let (mut reg, k) = registry_with_env();
        assert_eq!(reg.reserve_tab("t1", k), Ok(()));
        assert_eq!(reg.remove_tab("t1"), None);
        assert_eq!(reg.finish_tab("t1"), Err("unknown_tab".to_string()));
        assert!(!reg.has_tab("t1"));
        // The reservation is gone: the id can be created again.
        let k = key("p", Role::Owner);
        assert_eq!(reg.reserve_tab("t1", k), Ok(()));
    }

    #[test]
    fn a_released_reservation_frees_the_id() {
        let (mut reg, k) = registry_with_env();
        assert_eq!(reg.reserve_tab("t1", k.clone()), Ok(()));
        reg.release_tab("t1");
        assert!(!reg.has_tab("t1"));
        assert_eq!(reg.finish_tab("t1"), Err("unknown_tab".to_string()));
        assert_eq!(reg.reserve_tab("t1", k), Ok(()));
        // Releasing a live tab does not drop it.
        assert_eq!(reg.finish_tab("t1"), Ok(()));
        reg.release_tab("t1");
        assert!(reg.has_tab("t1"));
    }

    #[test]
    fn tab_state_serialises_with_the_contracts_camel_case_names() {
        let mut info = TabInfo::new("https://example.com/");
        info.title = "Example".to_string();
        info.loading = true;
        let json = serde_json::to_value(info.to_state("t1", true, false)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "tabId": "t1",
                "url": "https://example.com/",
                "title": "Example",
                "loading": true,
                "canGoBack": true,
                "canGoForward": false,
                "secure": true,
            })
        );
        let plain = TabInfo::new("http://example.com/").to_state("t2", false, false);
        assert!(!plain.secure);
        assert!(!plain.loading);
    }

    #[test]
    fn new_window_serialises_with_the_contracts_names() {
        let json = serde_json::to_value(NewWindow {
            from_tab_id: "t1".to_string(),
            url: "https://a.b/".to_string(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"fromTabId": "t1", "url": "https://a.b/"})
        );
    }
}
