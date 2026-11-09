use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Command;

// Compiled everywhere, like the scheduled-task helpers that read it (`task_is_absent`,
// `at_boot_create_args`): only Windows calls them, but every platform compiles them.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
const TASK_NAME: &str = "NucleOS Daemon";

/// PURE: an AppImage runs from a temporary mount, so registering that path would leave a dead
/// entry after the next launch.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn skip_registration(appimage: Option<&std::ffi::OsStr>) -> bool {
    appimage.is_some_and(|value| !value.is_empty())
}

pub fn ensure_registered(exe_path: &Path) -> std::io::Result<()> {
    if cfg!(all(unix, not(target_os = "macos")))
        && skip_registration(std::env::var_os("APPIMAGE").as_deref())
    {
        tracing::warn!("running from an AppImage: autostart is not registered");
        return Ok(());
    }
    reconcile(exe_path)
}

#[cfg(windows)]
fn reconcile(exe_path: &Path) -> std::io::Result<()> {
    let task = task_state(exe_path);
    if task == TaskPresence::QueryFailed {
        tracing::warn!("could not query the autostart task; leaving it as it is");
    }
    match registration_plan(task) {
        Registration::Keep => Ok(()),
        Registration::CreateAtLogin => register(exe_path),
        Registration::RepointExe => repoint(exe_path),
    }
}

#[cfg(not(windows))]
fn reconcile(exe_path: &Path) -> std::io::Result<()> {
    if is_registered(exe_path) {
        return Ok(());
    }
    register(exe_path)
}

/// When the daemon starts on its own. Never stored by NucleOS: the registered task (Windows) or the
/// user's linger flag (Linux) is the single source of truth, so a hand edit is never contradicted by
/// a second copy kept here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Starts when the owner logs in. The default.
    AtLogin,
    /// Starts with the machine, before any login.
    AtBoot,
}

/// What this platform can do for [`Mode::AtBoot`], and what the settings page should say about it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Capability {
    pub at_boot: bool,
    /// Whether switching to [`Mode::AtBoot`] needs the account's password, given once and never kept.
    pub needs_password: bool,
    /// Why `at_boot` is false, in words for the owner.
    pub why_not: Option<String>,
}

#[allow(dead_code)] // each build constructs only its own platform
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Windows,
    Macos,
    Linux,
}

impl Platform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// PURE: what each platform can do. `has_user_systemd` only matters on Linux, where linger is a
/// property of the systemd user manager.
fn capability_for(platform: Platform, has_user_systemd: bool) -> Capability {
    match platform {
        Platform::Windows => Capability {
            at_boot: true,
            needs_password: true,
            why_not: None,
        },
        Platform::Macos => Capability {
            at_boot: false,
            needs_password: false,
            why_not: Some(
                "macOS only starts a LaunchAgent at login, and a LaunchDaemon cannot reach the \
                 Keychain this app keeps its credentials in. To keep the machine usable without \
                 someone at the keyboard, turn on automatic login in the system settings."
                    .to_owned(),
            ),
        },
        Platform::Linux if has_user_systemd => Capability {
            at_boot: true,
            needs_password: false,
            why_not: None,
        },
        Platform::Linux => Capability {
            at_boot: false,
            needs_password: false,
            why_not: Some(
                "This session has no systemd user manager, which is what keeps a user's services \
                 running without a login."
                    .to_owned(),
            ),
        },
    }
}

/// PURE: an AppImage registers no unit (see [`skip_registration`]), and linger without a unit keeps
/// nothing running, so boot mode is not on offer there.
fn without_appimage(capability: Capability, appimage: Option<&std::ffi::OsStr>) -> Capability {
    if capability.at_boot && skip_registration(appimage) {
        Capability {
            at_boot: false,
            why_not: Some(
                "An AppImage runs from a temporary mount, so no autostart entry is registered \
                 for it and there is nothing for the system to start at boot."
                    .to_owned(),
            ),
            ..capability
        }
    } else {
        capability
    }
}

pub fn capability() -> Capability {
    let platform = Platform::current();
    let capability = capability_for(
        platform,
        platform == Platform::Linux && user_systemd_present(),
    );
    if platform == Platform::Linux {
        without_appimage(capability, std::env::var_os("APPIMAGE").as_deref())
    } else {
        capability
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn user_systemd_present() -> bool {
    has_user_systemd()
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn user_systemd_present() -> bool {
    false
}

/// Why [`set_mode`] did not change anything.
#[derive(Debug, PartialEq, Eq)]
pub enum SetModeError {
    /// This platform cannot do what was asked; the text says why.
    Unsupported(String),
    /// [`Mode::AtBoot`] needs the account's password here and none (or an empty one) came.
    PasswordRequired,
    /// The system tool refused. The text is its own words, with the password removed.
    Failed(String),
}

impl std::fmt::Display for SetModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(why) => f.write_str(why),
            Self::PasswordRequired => f.write_str("the account's password is required"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// What the registered task looks like, as far as re-registering cares.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskState {
    runs_this_exe: bool,
    at_boot: bool,
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum Registration {
    Keep,
    CreateAtLogin,
    RepointExe,
}

/// What looking for the task came back with. "Could not look" is not "no task": treating a failed
/// query as absent would overwrite a boot task with the login one and lose its stored password,
/// which only the owner can give back.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskPresence {
    Absent,
    QueryFailed,
    Present(TaskState),
}

/// PURE: what `ensure_registered` does about the task it found.
///
/// A new registration used to overwrite the task with the login one, which would silently undo
/// [`Mode::AtBoot`] on the next start. A boot task cannot be recreated here, because that needs
/// the password NucleOS never keeps, so one that points at a moved executable only has its command
/// changed and keeps its trigger and its credentials. A failed query changes nothing.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn registration_plan(task: TaskPresence) -> Registration {
    match task {
        TaskPresence::QueryFailed => Registration::Keep,
        TaskPresence::Present(TaskState {
            runs_this_exe: true,
            ..
        }) => Registration::Keep,
        TaskPresence::Present(TaskState { at_boot: true, .. }) => Registration::RepointExe,
        TaskPresence::Present(_) | TaskPresence::Absent => Registration::CreateAtLogin,
    }
}

/// PURE: whether a listing of every task proves ours is not there. `schtasks /Query /TN` exits
/// non-zero for a missing task and for a broken query alike, and its message is localized, so the
/// only locale-safe proof of absence is a listing that succeeded without the task's name in it.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn task_is_absent(listing: Option<&str>) -> bool {
    listing.is_some_and(|listing| !listing.contains(TASK_NAME))
}

/// PURE: the failure `set_mode` reports for what `schtasks` (or the work before it) said, with the
/// password taken out of both streams.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn schtasks_failure(stderr: &str, stdout: &str, password: &str) -> SetModeError {
    SetModeError::Failed(scrub(&format!("{stderr} {stdout}"), password))
}

/// PURE: whether a task's XML has a boot trigger.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn xml_is_at_boot(xml: &str) -> bool {
    xml.contains("<BootTrigger>")
}

/// PURE: `schtasks` arguments that replace only the command of the existing task. The command is
/// the same one `task_document` writes, so a repointed boot task still runs windowless and
/// supervised and `xml_matches_task` accepts it on the next start.
#[cfg(any(windows, test))]
fn repoint_args(exe_path: &Path) -> Vec<String> {
    vec![
        "/Change".to_owned(),
        "/TN".to_owned(),
        TASK_NAME.to_owned(),
        "/TR".to_owned(),
        format!(
            "\"{}\" --headless \"{}\" {}",
            conhost_path(),
            exe_path.to_string_lossy(),
            crate::supervise::SUPERVISE_FLAG
        ),
    ]
}

/// PURE: `schtasks` arguments that create the boot task. The password is the one argument that
/// must never be logged or kept; the caller passes the returned vector straight to the process.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn at_boot_create_args(xml_path: &Path, user: &str, password: &str) -> Vec<String> {
    vec![
        "/Create".to_owned(),
        "/TN".to_owned(),
        TASK_NAME.to_owned(),
        "/XML".to_owned(),
        xml_path.to_string_lossy().into_owned(),
        "/RU".to_owned(),
        user.to_owned(),
        "/RP".to_owned(),
        password.to_owned(),
        "/F".to_owned(),
    ]
}

/// PURE: a tool's complaint with the password taken out of it, for the response and the log.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn scrub(message: &str, password: &str) -> String {
    if password.is_empty() {
        return message.trim().to_owned();
    }
    message.replace(password, "***").trim().to_owned()
}

/// PURE: the mode a `loginctl show-user -p Linger` answer describes.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn mode_from_linger(output: &str) -> Mode {
    if output.lines().any(|line| line.trim() == "Linger=yes") {
        Mode::AtBoot
    } else {
        Mode::AtLogin
    }
}

/// PURE: the one `loginctl` verb that makes `mode` true.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn linger_verb(mode: Mode) -> &'static str {
    match mode {
        Mode::AtBoot => "enable-linger",
        Mode::AtLogin => "disable-linger",
    }
}

/// PURE: escapes a value for an XML text node.
///
/// The task XML was built by interpolation, and `&` is legal in a Windows path — a repository under
/// `C:\Dev & Ops\` produced malformed XML, `schtasks /Create` rejected it, and autostart degraded to
/// a single warning line nobody reads. `<` is not reachable through a path today, which is exactly
/// the sort of reasoning that stops being true when the next caller passes something else.
#[cfg(any(windows, target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The registered task's XML, or why there is none.
#[cfg(windows)]
enum TaskLookup {
    Found(String),
    Absent,
    QueryFailed,
}

#[cfg(windows)]
fn query_task_xml() -> TaskLookup {
    let Ok(output) = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/XML", "ONE"])
        .output()
    else {
        return TaskLookup::QueryFailed;
    };
    if !output.status.success() {
        let listing = Command::new("schtasks")
            .args(["/Query", "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .filter(|listing| listing.status.success())
            .map(|listing| String::from_utf8_lossy(&listing.stdout).into_owned());
        return if task_is_absent(listing.as_deref()) {
            TaskLookup::Absent
        } else {
            TaskLookup::QueryFailed
        };
    }
    // schtasks emits UTF-16LE here; comparing bytes would never match. Lossy is fine — a path that
    // does not survive the round trip is not one we would match anyway.
    TaskLookup::Found(
        output
            .stdout
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>()
            .iter()
            .map(|unit| char::from_u32(u32::from(*unit)).unwrap_or('\u{fffd}'))
            .collect(),
    )
}

/// Whether the registered task, if any, is one that actually launches THIS executable, and its
/// trigger.
///
/// The name alone was the whole check, and the name is not a fact about the task: anything already
/// called "NucleOS Daemon" satisfied it forever, whatever it ran. The benign version of that is an
/// upgrade or a move leaving a task pointing at a path that no longer exists — silently, since
/// nothing looked. The unpleasant version is a same-user process pre-creating that name and owning
/// a logon-persistence slot this daemon would then refuse to correct.
#[cfg(windows)]
fn task_state(exe_path: &Path) -> TaskPresence {
    match query_task_xml() {
        TaskLookup::Found(xml) => TaskPresence::Present(TaskState {
            runs_this_exe: xml_matches_task(&xml, exe_path),
            at_boot: xml_is_at_boot(&xml),
        }),
        TaskLookup::Absent => TaskPresence::Absent,
        TaskLookup::QueryFailed => TaskPresence::QueryFailed,
    }
}

/// The mode the machine is in now.
#[cfg(windows)]
pub fn current_mode() -> Mode {
    match query_task_xml() {
        TaskLookup::Found(xml) if xml_is_at_boot(&xml) => Mode::AtBoot,
        _ => Mode::AtLogin,
    }
}

#[cfg(windows)]
fn windows_user() -> std::io::Result<String> {
    user_id_from(
        std::env::var("USERDOMAIN").ok().as_deref(),
        std::env::var("USERNAME").ok().as_deref(),
    )
    .ok_or_else(|| std::io::Error::other("no USERNAME to run the task as"))
}

/// Switches the mode. The password, when the platform wants one, goes to the system tool once and
/// is neither stored nor logged.
#[cfg(windows)]
pub fn set_mode(mode: Mode, password: Option<&str>, exe_path: &Path) -> Result<(), SetModeError> {
    match mode {
        Mode::AtLogin => {
            register(exe_path).map_err(|error| SetModeError::Failed(error.to_string()))
        }
        Mode::AtBoot => {
            let password = password
                .filter(|password| !password.is_empty())
                .ok_or(SetModeError::PasswordRequired)?;
            let failed = |error: std::io::Error| schtasks_failure(&error.to_string(), "", password);
            let user = windows_user().map_err(failed)?;
            let xml_path = task_xml_temp_path();
            write_utf16_xml(&xml_path, &at_boot_task_xml(exe_path, &user)).map_err(failed)?;
            let output = Command::new("schtasks")
                .args(at_boot_create_args(&xml_path, &user, password))
                .stdin(std::process::Stdio::null())
                .output();
            let _ = std::fs::remove_file(&xml_path);
            let output = output.map_err(failed)?;
            if output.status.success() {
                return Ok(());
            }
            Err(schtasks_failure(
                &String::from_utf8_lossy(&output.stderr),
                &String::from_utf8_lossy(&output.stdout),
                password,
            ))
        }
    }
}

#[cfg(windows)]
fn repoint(exe_path: &Path) -> std::io::Result<()> {
    let status = Command::new("schtasks")
        .args(repoint_args(exe_path))
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("schtasks /change failed"))
    }
}

/// Whether a queried task XML launches `exe_path` AND carries the settings this build registers.
/// A task from an older build (default 72h limit, or the inert `RestartOnFailure` block, no
/// `--supervise`) names the same exe, so the exe alone would keep it forever; requiring `PT0S` and
/// the supervising flag at the end of the action's arguments makes it re-register.
/// `--headless` is required too: a task whose action runs the exe directly (the pre-conhost format)
/// opens a console window, so it must not count as registered and is re-registered with `/F`.
#[cfg(any(windows, test))]
fn xml_matches_task(xml: &str, exe_path: &Path) -> bool {
    let lower = xml.to_lowercase();
    // The action must be the conhost launcher and the exe must sit in its arguments; the exe path
    // appearing anywhere else in the document (a description, a forged action) proves nothing.
    // If `SystemRoot` differs at the next start, the task simply re-registers.
    let (Some(command), Some(arguments)) = (
        element_text(&lower, "command"),
        element_text(&lower, "arguments"),
    ) else {
        return false;
    };
    let escaped_conhost = xml_escape(&conhost_path()).to_lowercase();
    let command_is_conhost = command == escaped_conhost
        || command == escaped_conhost.replace("&apos;", "'")
        || command == conhost_path().to_lowercase();
    // `schtasks /Query` may echo an apostrophe either escaped or literal.
    let escaped_exe = xml_escape(&exe_path.to_string_lossy()).to_lowercase();
    let exe_in_arguments =
        arguments.contains(&escaped_exe) || arguments.contains(&escaped_exe.replace("&apos;", "'"));
    command_is_conhost
        && exe_in_arguments
        && lower.contains("--headless")
        && lower.contains("pt0s")
        && arguments.ends_with(&format!(" {}", crate::supervise::SUPERVISE_FLAG))
}

/// PURE: the trimmed text of the first `<tag>…</tag>` in already-lowercased XML.
#[cfg(any(windows, test))]
fn element_text<'a>(lower_xml: &'a str, tag: &str) -> Option<&'a str> {
    let start = lower_xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + lower_xml[start..].find(&format!("</{tag}>"))?;
    Some(lower_xml[start..end].trim())
}

#[cfg(windows)]
fn register(exe_path: &Path) -> std::io::Result<()> {
    // Without a user id the task means "any user logs on", which only an admin may create.
    let Ok(user) = windows_user() else {
        tracing::warn!("no USERNAME in the environment; autostart is not registered");
        return Ok(());
    };
    let xml = task_xml(exe_path, &user);
    let xml_path = task_xml_temp_path();
    write_utf16_xml(&xml_path, &xml)?;
    let output = Command::new("schtasks")
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&xml_path)
        .arg("/F")
        .stdin(std::process::Stdio::null())
        .output();
    let _ = std::fs::remove_file(&xml_path);
    let output = output?;
    if output.status.success() {
        Ok(())
    } else {
        // schtasks prints its reason on stderr in the console code page; lossy keeps the ASCII.
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(std::io::Error::other(register_failure_message(
            output.status.code(),
            stderr.trim(),
        )))
    }
}

/// PURE: the error for a failed `schtasks /Create`: the cause schtasks gave and, for a denial,
/// what to do about it. The old text ("schtasks /create failed") hid the reason, so an unelevated
/// "Acesso negado" looked like a random failure.
#[cfg(any(windows, test))]
fn register_failure_message(code: Option<i32>, stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    let denied = lower.contains("acesso negado") || lower.contains("access is denied");
    let code = code.map_or_else(|| "none".to_string(), |c| c.to_string());
    let mut msg = format!(
        "schtasks /Create for the \"NucleOS Daemon\" logon task failed (exit {code}): {stderr}"
    );
    if denied {
        msg.push_str(
            " -- access denied: an existing task of that name was probably created from an \
             elevated shell and cannot be replaced unelevated. Delete it once from an elevated \
             prompt (schtasks /Delete /TN \"NucleOS Daemon\" /F); the daemon then re-registers it \
             for the current user without elevation.",
        );
    }
    msg
}

/// PURE: `DOMAIN\name` from the two environment values, `name` alone without a domain, `None`
/// when the name is missing or blank.
#[cfg(any(windows, test))]
fn user_id_from(domain: Option<&str>, name: Option<&str>) -> Option<String> {
    let name = name.map(str::trim).filter(|n| !n.is_empty())?;
    match domain.map(str::trim).filter(|d| !d.is_empty()) {
        Some(domain) => Some(format!("{domain}\\{name}")),
        None => Some(name.to_string()),
    }
}

/// The directory the daemon runs from. A dev build lives at `<repo_root>/target/{debug,release}/`
/// or `<repo_root>/core/target/{debug,release}/`, and there the repo root is the answer. Every
/// other layout uses the exe's own directory: installed ones (`%LOCALAPPDATA%\NucleOS\`,
/// `/usr/bin`, `NucleOS.app/Contents/MacOS`), and also the shared `.cargo-target-*` directories
/// and `.nucleos-run`, which are not inside a repo. The daemon reads nothing repo-relative except
/// the one-time copy of a legacy `.ai/`, so the exe's directory is a sound default — and better
/// than omitting `<WorkingDirectory>`, which Task Scheduler does NOT default to the exe's own
/// directory on its own.
fn working_directory_for(exe_path: &Path) -> PathBuf {
    let name_is = |p: &Path, names: &[&str]| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| names.contains(&n))
    };
    let own_dir = exe_path.parent();
    let dev_root = own_dir
        .filter(|profile| name_is(profile, &["debug", "release"]))
        .and_then(Path::parent)
        .filter(|target| name_is(target, &["target"]))
        .and_then(Path::parent)
        .map(|up| match up.parent() {
            Some(parent) if name_is(up, &["core"]) => parent,
            _ => up,
        });
    dev_root
        .or(own_dir)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The console host the task launches the daemon through. `conhost.exe --headless <exe>` runs a
/// console-subsystem program with no window, so the daemon binary's subsystem stays untouched.
#[cfg(windows)]
fn conhost_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\conhost.exe")
}

/// Test-only stand-in so the task XML is deterministic on every host.
#[cfg(all(test, not(windows)))]
fn conhost_path() -> String {
    r"C:\Windows\System32\conhost.exe".to_string()
}

/// PURE: the login task, bound to `user` (`DOMAIN\name`). A `LogonTrigger` and a `Principal` with
/// no `UserId` mean "any user", and registering that needs admin rights.
#[cfg(any(windows, test))]
fn task_xml(exe_path: &Path, user: &str) -> String {
    task_xml_for(exe_path, Some(user))
}

/// The task document for `user` (`DOMAIN\name`), written on the logon trigger AND the principal:
/// a trigger without `<UserId>` fires for every user's logon, which Task Scheduler only lets an
/// administrator register. The run level stays `LeastPrivilege`, never SYSTEM or highest.
#[cfg(any(windows, test))]
fn task_xml_for(exe_path: &Path, user: Option<&str>) -> String {
    let user_line = user
        .map(|u| format!("\n      <UserId>{}</UserId>", xml_escape(u)))
        .unwrap_or_default();
    let user_tag = user
        .map(|u| format!("<UserId>{}</UserId>\n      ", xml_escape(u)))
        .unwrap_or_default();
    task_document(
        exe_path,
        &format!("<LogonTrigger>\n      <Enabled>true</Enabled>{user_line}\n    </LogonTrigger>"),
        &format!("{user_tag}<LogonType>InteractiveToken</LogonType>"),
    )
}

/// PURE: the boot task. `LogonType` `Password` is what lets it run with nobody logged in; `S4U` is
/// out because that logon has no access to the Credential Manager, where the daemon keeps its
/// token. The password is not an input here: it reaches `schtasks` as an argument and nowhere else.
#[cfg(any(windows, test))]
fn at_boot_task_xml(exe_path: &Path, user: &str) -> String {
    task_document(
        exe_path,
        "<BootTrigger>\n      <Enabled>true</Enabled>\n    </BootTrigger>",
        &format!(
            "<UserId>{}</UserId>\n      <LogonType>Password</LogonType>",
            xml_escape(user)
        ),
    )
}

#[cfg(any(windows, test))]
fn task_document(exe_path: &Path, trigger: &str, logon: &str) -> String {
    let working_directory = working_directory_for(exe_path);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    {trigger}
  </Triggers>
  <Principals>
    <Principal id="Author">
      {logon}
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Hidden>true</Hidden>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
      <Arguments>--headless "{}" {}</Arguments>
      <WorkingDirectory>{}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>"#,
        xml_escape(&conhost_path()),
        xml_escape(&exe_path.to_string_lossy()),
        crate::supervise::SUPERVISE_FLAG,
        xml_escape(&working_directory.to_string_lossy())
    )
}

#[cfg(any(windows, test))]
fn write_utf16_xml(path: &Path, xml: &str) -> std::io::Result<()> {
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // UTF-16LE BOM
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    crate::storage::write_atomic(path, &bytes)
}

/// A per-registration XML path avoids concurrent registrations overwriting each other's task file.
#[cfg(any(windows, test))]
fn task_xml_temp_path() -> PathBuf {
    static NEXT_TASK_XML_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let serial = NEXT_TASK_XML_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nucleos-daemon-task-{}-{nonce}-{serial}.xml",
        std::process::id()
    ))
}

/// The LaunchAgent's label, which is also its file name under `~/Library/LaunchAgents`.
#[cfg(any(target_os = "macos", test))]
const LAUNCH_AGENT_LABEL: &str = "dev.nucleos.core";

/// The systemd user unit's name.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
const USER_UNIT: &str = "nucleos-core.service";

/// PURE over the file system: whether `path` holds exactly the entry this executable would write.
///
/// The rule `is_registered` keeps on Windows: registered means "launches THIS executable". Equality
/// rather than a search for the path, because every byte of these files is ours; a moved
/// repository, an edited entry or an older format all read as not registered and are rewritten.
#[cfg(any(unix, test))]
fn entry_is_current(path: &Path, expected: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| content == expected)
}

/// A path as one line of UTF-8, or the reason no autostart entry can name it.
#[cfg(any(unix, test))]
fn one_line(path: &Path) -> std::io::Result<String> {
    let Some(text) = path.to_str() else {
        return Err(std::io::Error::other(format!(
            "{path:?} is not UTF-8, so no autostart entry can name it"
        )));
    };
    if text.contains(['\n', '\r']) {
        return Err(std::io::Error::other(format!(
            "{text:?} holds a line break, so no autostart entry can name it"
        )));
    }
    Ok(text.to_owned())
}

#[cfg(unix)]
fn write_entry(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::storage::write_atomic(path, content.as_bytes())
}

/// PURE: the macOS LaunchAgent. `exe` and `workdir` come from [`one_line`].
#[cfg(any(target_os = "macos", test))]
fn launch_agent_plist(exe: &str, workdir: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>WorkingDirectory</key><string>{}</string>
  <key>RunAtLoad</key><true/>
  <key>AbandonProcessGroup</key><true/>
</dict>
</plist>
"#,
        xml_escape(exe),
        xml_escape(workdir)
    )
}

/// PURE: the systemd user unit. Unlike the Windows task, whose `RestartOnFailure` was measured to
/// restart nothing on a non-zero exit, this unit really restarts on failure (`Restart=on-failure`,
/// three restarts at most, one minute apart).
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn systemd_unit(exe: &str, workdir: &str) -> String {
    format!(
        r#"[Unit]
Description=NucleOS daemon
StartLimitIntervalSec=600
StartLimitBurst=3

[Service]
ExecStart={}
WorkingDirectory={}
Restart=on-failure
RestartSec=60

[Install]
WantedBy=default.target
"#,
        systemd_quote(exe),
        workdir.replace('%', "%%")
    )
}

/// PURE: one quoted `ExecStart=` word. Inside quotes systemd reads C-style backslash escapes, and
/// everywhere it expands `%` specifiers and `$` variables.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn systemd_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

/// PURE: the XDG autostart entry, for a Linux session with no systemd user manager.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn xdg_desktop_entry(exe: &str, workdir: &str) -> String {
    format!(
        r#"[Desktop Entry]
Type=Application
Name=NucleOS daemon
Exec={}
Path={}
NoDisplay=true
X-GNOME-Autostart-enabled=true
"#,
        desktop_quote(exe),
        workdir.replace('\\', "\\\\")
    )
}

/// PURE: the program of an `Exec=` line. The Desktop Entry spec quotes an argument in double quotes
/// with `"`, backquote, `$` and backslash escaped by a backslash, THEN applies the file's string
/// escapes (a backslash is written twice), and reserves `%` for field codes.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn desktop_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    let escaped = quoted.replace('\\', "\\\\").replace('%', "%%");
    format!("\"{escaped}\"")
}

/// macOS: the LaunchAgent that launchd loads at the next login.
///
/// Deliberately never `launchctl bootstrap`: the agent has `RunAtLoad`, so loading it now would
/// start a second primary daemon beside this one, and that daemon marks this one's live runs
/// `interrupted` (`runs::reconcile_orphaned_runs`) before it fails to bind the port. `schtasks
/// /Create` does not start the Windows task either (portability spec, D5).
#[cfg(target_os = "macos")]
fn launch_agent_path() -> std::io::Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| std::io::Error::other("no home directory to hold a LaunchAgent"))?;
    Ok(base
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launch_agent_for(exe_path: &Path) -> std::io::Result<String> {
    let exe = one_line(exe_path)?;
    let workdir = one_line(&working_directory_for(exe_path))?;
    Ok(launch_agent_plist(&exe, &workdir))
}

#[cfg(target_os = "macos")]
fn is_registered(exe_path: &Path) -> bool {
    match (launch_agent_path(), launch_agent_for(exe_path)) {
        (Ok(path), Ok(expected)) => entry_is_current(&path, &expected),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn register(exe_path: &Path) -> std::io::Result<()> {
    write_entry(&launch_agent_path()?, &launch_agent_for(exe_path)?)
}

/// Linux: whether this session has a systemd user manager to hold a unit.
#[cfg(all(unix, not(target_os = "macos")))]
fn has_user_systemd() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
/// The other Linux backend's existing entries, which must be removed before installing ours.
///
/// Leaving a systemd unit and an XDG autostart entry installed lets two primary daemons start at
/// login; the second can mark the first daemon's runs interrupted before it fails to bind.
fn other_backend_entries(config: &Path, systemd: bool) -> Vec<PathBuf> {
    let candidates = if systemd {
        vec![config.join("autostart").join("nucleos-core.desktop")]
    } else {
        let user = config.join("systemd").join("user");
        vec![
            user.join(USER_UNIT),
            user.join("default.target.wants").join(USER_UNIT),
        ]
    };
    candidates
        .into_iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .collect()
}

/// Linux: where the entry lives and what it says. A systemd user unit when there is a user
/// manager, else an XDG autostart entry the desktop session launches at login.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_entry(exe_path: &Path, systemd: bool) -> std::io::Result<(PathBuf, String)> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| std::io::Error::other("no home directory to hold an autostart entry"))?;
    let exe = one_line(exe_path)?;
    let workdir = one_line(&working_directory_for(exe_path))?;
    let config = base.config_dir();
    let systemd_user = config.join("systemd").join("user");
    Ok(if systemd {
        (systemd_user.join(USER_UNIT), systemd_unit(&exe, &workdir))
    } else {
        (
            config.join("autostart").join("nucleos-core.desktop"),
            xdg_desktop_entry(&exe, &workdir),
        )
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn is_registered(exe_path: &Path) -> bool {
    let systemd = has_user_systemd();
    let Some(base) = directories::BaseDirs::new() else {
        return false;
    };
    if !other_backend_entries(base.config_dir(), systemd).is_empty() {
        return false;
    }
    let Ok((path, expected)) = linux_entry(exe_path, systemd) else {
        return false;
    };
    if !entry_is_current(&path, &expected) {
        return false;
    }
    if !systemd {
        return true;
    }
    // A unit file nobody enabled launches nothing at login.
    Command::new("systemctl")
        .args(["--user", "is-enabled", USER_UNIT])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "enabled")
}

/// `enable` and never `enable --now`, for the reason `launch_agent_path` gives: starting the unit
/// now would run a second primary daemon beside this one (portability spec, D5).
#[cfg(all(unix, not(target_os = "macos")))]
fn register(exe_path: &Path) -> std::io::Result<()> {
    let systemd = has_user_systemd();
    let base = directories::BaseDirs::new()
        .ok_or_else(|| std::io::Error::other("no home directory to hold an autostart entry"))?;
    for other in other_backend_entries(base.config_dir(), systemd) {
        match std::fs::remove_file(other) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let (path, content) = linux_entry(exe_path, systemd)?;
    write_entry(&path, &content)?;
    if systemd {
        systemctl_user(&["daemon-reload"])?;
        systemctl_user(&["enable", USER_UNIT])?;
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemctl_user(args: &[&str]) -> std::io::Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl --user {} failed",
            args.join(" ")
        )))
    }
}

/// macOS has no boot mode: a LaunchAgent needs a login. See [`capability_for`].
#[cfg(target_os = "macos")]
pub fn current_mode() -> Mode {
    Mode::AtLogin
}

#[cfg(target_os = "macos")]
pub fn set_mode(mode: Mode, _password: Option<&str>, _exe_path: &Path) -> Result<(), SetModeError> {
    match mode {
        Mode::AtLogin => Ok(()),
        Mode::AtBoot => Err(SetModeError::Unsupported(
            capability_for(Platform::Macos, false)
                .why_not
                .unwrap_or_default(),
        )),
    }
}

/// Linux: linger is what keeps the user's systemd manager, and so the unit, alive without a login.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn current_mode() -> Mode {
    let Some(user) = std::env::var("USER").ok().filter(|user| !user.is_empty()) else {
        return Mode::AtLogin;
    };
    Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| mode_from_linger(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or(Mode::AtLogin)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn set_mode(mode: Mode, _password: Option<&str>, exe_path: &Path) -> Result<(), SetModeError> {
    let capability = capability();
    if mode == Mode::AtBoot && !capability.at_boot {
        return Err(SetModeError::Unsupported(
            capability.why_not.unwrap_or_default(),
        ));
    }
    if mode == Mode::AtBoot {
        // Linger starts a unit that exists; make sure there is one.
        ensure_registered(exe_path).map_err(|error| SetModeError::Failed(error.to_string()))?;
    }
    let output = Command::new("loginctl")
        .arg(linger_verb(mode))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| SetModeError::Failed(format!("loginctl: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(SetModeError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The login task for a fixed user, so the XML is the same on every host.
    fn task_xml(exe: &Path) -> String {
        super::task_xml(exe, r"PC\me")
    }

    #[test]
    fn the_login_task_names_the_user_in_trigger_and_principal_escaped() {
        let xml = super::task_xml(Path::new(r"C:\Apps\nucleos-core.exe"), r"A&B\Dev <1>");
        let id = r"<UserId>A&amp;B\Dev &lt;1&gt;</UserId>";
        assert_eq!(xml.matches(id).count(), 2, "{xml}");
        let between = |open: &str, close: &str| {
            let start = xml.find(open).unwrap();
            &xml[start..start + xml[start..].find(close).unwrap()]
        };
        assert!(
            between("<LogonTrigger>", "</LogonTrigger>").contains(id),
            "{xml}"
        );
        assert!(between("<Principal ", "</Principal>").contains(id), "{xml}");
    }

    #[cfg(windows)]
    #[test]
    fn task_xml_embeds_the_exe_path_and_repo_root_working_directory() {
        let xml = task_xml(Path::new(
            r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe",
        ));
        assert!(xml.contains(r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe"));
        assert!(!xml.contains("<RestartOnFailure>"));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains(r"<WorkingDirectory>C:\Projects\nucleos</WorkingDirectory>"));
    }

    /// The daemon is a long-lived process: the task must never be killed by the default 72h limit.
    /// `<Hidden>true</Hidden>` hides the task in the Task Scheduler UI; the console window is
    /// suppressed separately, by launching through `conhost.exe --headless`. `RestartOnFailure` is
    /// gone: measured, it restarts nothing here (the Task Scheduler ignores a non-zero exit code
    /// and `conhost --headless` returns 0), and `--supervise` does the restarting now, so a block
    /// that could one day start working would run two restart loops on top of each other.
    #[test]
    fn task_xml_has_no_time_limit_is_hidden_and_leaves_restarting_to_the_supervisor() {
        let xml = task_xml(Path::new(
            r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe",
        ));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Hidden>true</Hidden>"));
        assert!(!xml.contains("RestartOnFailure"));
        assert!(!xml.contains("<Count>"));
    }

    /// A logon trigger with no `<UserId>` fires for ANY user's logon, and Task Scheduler only lets an
    /// administrator register that ("Acesso negado" unelevated). Scoping trigger and principal to
    /// the current user is what makes the unelevated registration succeed.
    #[test]
    fn the_task_is_scoped_to_one_user_so_an_unelevated_registration_is_allowed() {
        let xml = task_xml_for(Path::new(r"C:\x\nucleos-core.exe"), Some(r"PC\Paulo"));
        assert!(xml.contains(
            "<LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>PC\\Paulo</UserId>"
        ));
        assert!(xml.contains("<Principal id=\"Author\">\n      <UserId>PC\\Paulo</UserId>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(!xml.contains("HighestAvailable"));
        assert!(!xml.contains("S-1-5-18") && !xml.to_lowercase().contains("system</userid>"));
    }

    #[test]
    fn a_user_name_with_xml_syntax_is_escaped() {
        let xml = task_xml_for(Path::new(r"C:\x\nucleos-core.exe"), Some(r"A&B\o<n>e"));
        assert!(xml.contains(r"<UserId>A&amp;B\o&lt;n&gt;e</UserId>"));
    }

    #[test]
    fn without_a_known_user_the_task_has_no_user_element() {
        let xml = task_xml_for(Path::new(r"C:\x\nucleos-core.exe"), None);
        assert!(!xml.contains("<UserId>"));
        assert!(xml.contains("<LogonTrigger>"));
    }

    #[test]
    fn current_user_joins_domain_and_name_and_ignores_blanks() {
        assert_eq!(
            user_id_from(Some("PC"), Some("Paulo")).as_deref(),
            Some(r"PC\Paulo")
        );
        assert_eq!(user_id_from(None, Some("Paulo")).as_deref(), Some("Paulo"));
        assert_eq!(user_id_from(Some("PC"), Some("  ")), None);
        assert_eq!(user_id_from(Some("PC"), None), None);
    }

    #[test]
    fn a_denied_registration_explains_how_to_get_unblocked() {
        let msg = register_failure_message(Some(1), "ERRO: Acesso negado.");
        assert!(msg.contains("Acesso negado"));
        assert!(msg.contains("NucleOS Daemon"));
        assert!(msg.contains("elevated"), "{msg}");
        let other = register_failure_message(Some(1), "boom");
        assert!(other.contains("boom") && !other.contains("elevated"));
    }

    #[test]
    fn the_task_runs_the_daemon_through_conhost_headless_in_supervising_mode() {
        let exe = r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe";
        let xml = task_xml(Path::new(exe));
        let command = format!("<Command>{}</Command>", conhost_path());
        assert!(xml.contains(&command));
        assert!(xml.contains(&format!(
            "<Arguments>--headless \"{exe}\" --supervise</Arguments>"
        )));
        assert!(!xml.contains(&format!("<Command>{exe}</Command>")));
        #[cfg(not(windows))]
        assert!(xml.contains(r"<Command>C:\Windows\System32\conhost.exe</Command>"));
    }

    /// The task as the previous build registered it: windowless through conhost, restart block and
    /// all, but without `--supervise`. It names the right exe and carries every other setting, so
    /// only the missing flag can make it re-register.
    #[test]
    fn a_task_registered_before_the_supervisor_is_re_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let current = task_xml(exe);
        let previous = current
            .replace(" --supervise", "")
            .replace(
                "  </Settings>",
                "    <RestartOnFailure>\n      <Interval>PT1M</Interval>\n      <Count>99</Count>\n    </RestartOnFailure>\n  </Settings>",
            );
        assert_ne!(
            previous, current,
            "the fixture must actually change the task"
        );
        assert!(!previous.contains("--supervise"));
        assert!(previous.contains("<Count>99</Count>"));
        assert!(!xml_matches_task(&previous, exe));
        assert!(xml_matches_task(&current, exe));
    }

    /// `--supervise` somewhere else in the document (a description) is not the action's argument.
    #[test]
    fn a_task_with_supervise_outside_its_arguments_is_not_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let genuine = task_xml(exe);
        let forged = genuine
            .replace(" --supervise</Arguments>", "</Arguments>")
            .replace("</Exec>", "<Description>--supervise</Description></Exec>");
        assert_ne!(forged, genuine, "the fixture must actually change the task");
        assert!(forged.contains("--supervise"));
        assert!(!xml_matches_task(&forged, exe));
    }

    /// The freshly generated task needs no restart block to count as registered.
    #[test]
    fn the_supervising_task_counts_as_registered_without_a_restart_block() {
        for raw in [r"C:\x\nucleos-core.exe", r"C:\Bob's & Co\nucleos-core.exe"] {
            let exe = Path::new(raw);
            let xml = task_xml(exe);
            assert!(!xml.contains("RestartOnFailure"), "{raw}");
            assert!(xml_matches_task(&xml, exe), "{raw}");
        }
    }

    #[test]
    fn a_task_registered_before_conhost_headless_is_re_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let old = r"<Task><Settings><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings><Actions><Exec><Command>C:\x\nucleos-core.exe</Command><WorkingDirectory>C:\x</WorkingDirectory></Exec></Actions></Task>";
        assert!(!xml_matches_task(old, exe));
        assert!(xml_matches_task(&task_xml(exe), exe));
    }

    #[test]
    fn an_old_task_without_the_current_settings_is_not_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let current = task_xml(exe);
        assert!(xml_matches_task(&current, exe));
        let old = r"<Task><Exec><Command>C:\x\nucleos-core.exe</Command></Exec></Task>";
        assert!(!xml_matches_task(old, exe));
        assert!(!xml_matches_task(
            &current,
            Path::new(r"C:\other\nucleos-core.exe")
        ));
    }

    /// RED for the `<Command>` check: the exe path and `--headless` appearing anywhere in the XML
    /// is not enough, the action must be the conhost launcher.
    #[test]
    fn a_task_whose_command_is_not_conhost_is_not_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let genuine = task_xml(exe);
        let forged = genuine.replace(
            &format!("<Command>{}</Command>", conhost_path()),
            r"<Command>C:\Windows\System32\cmd.exe</Command>",
        );
        assert_ne!(
            forged, genuine,
            "the fixture must actually change the command"
        );
        assert!(forged.contains("--headless"));
        assert!(forged.contains(r"C:\x\nucleos-core.exe"));
        assert!(!xml_matches_task(&forged, exe));
    }

    /// RED for the `<Arguments>` check: conhost is the command, but the exe path sits in another
    /// element (here a `<Description>`), so the task would not launch this exe.
    #[test]
    fn a_task_with_conhost_but_the_exe_outside_arguments_is_not_registered() {
        let exe = Path::new(r"C:\x\nucleos-core.exe");
        let genuine = task_xml(exe);
        let forged = genuine
            .replace(
                &format!(
                    "<Arguments>--headless \"{}\" --supervise</Arguments>",
                    exe.display()
                ),
                "<Arguments>--headless --supervise</Arguments>",
            )
            .replace(
                "</Exec>",
                r"<Description>C:\x\nucleos-core.exe</Description></Exec>",
            );
        assert_ne!(
            forged, genuine,
            "the fixture must actually change the arguments"
        );
        assert!(forged.contains(&format!("<Command>{}</Command>", conhost_path())));
        assert!(forged.contains(r"C:\x\nucleos-core.exe"));
        assert!(!xml_matches_task(&forged, exe));
    }

    /// Must pass today and after the fix: the XML this build generates counts as registered, also
    /// with an apostrophe in the path (`task_xml` writes `&apos;`, as `schtasks /Query` echoes it).
    #[test]
    fn the_generated_task_xml_counts_as_registered_even_with_escapes() {
        for raw in [r"C:\x\nucleos-core.exe", r"C:\Bob's & Co\nucleos-core.exe"] {
            let exe = Path::new(raw);
            assert!(xml_matches_task(&task_xml(exe), exe), "{raw}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_working_directory_is_four_levels_above_a_posix_exe() {
        assert_eq!(
            working_directory_for(Path::new("/home/me/nucleos/core/target/debug/nucleos-core")),
            Path::new("/home/me/nucleos")
        );
    }

    /// `&` is legal in a Windows path, and the XML was built by interpolation — a repository under
    /// `C:\Dev & Ops\` produced a document `schtasks /Create` rejects, after which autostart
    /// degraded to one warning line and never worked again.
    #[test]
    fn a_path_with_xml_syntax_in_it_still_produces_a_valid_document() {
        let xml = task_xml(Path::new(
            r"C:\Dev & Ops\nucleos\core\target\debug\nucleos-core.exe",
        ));

        assert!(xml.contains(r"C:\Dev &amp; Ops\nucleos\core\target\debug\nucleos-core.exe"));
        assert!(
            xml.contains(
                r#"<Arguments>--headless "C:\Dev &amp; Ops\nucleos\core\target\debug\nucleos-core.exe" --supervise</Arguments>"#
            ),
            "the escaped path must sit, quoted, in Arguments"
        );
        assert!(
            !xml.contains("Dev & Ops"),
            "the raw ampersand must not reach the document"
        );
        // One unescaped `&` is the whole failure; there must be none left outside an entity.
        for (index, _) in xml.match_indices('&') {
            let tail = &xml[index..];
            assert!(
                tail.starts_with("&amp;")
                    || tail.starts_with("&lt;")
                    || tail.starts_with("&gt;")
                    || tail.starts_with("&quot;")
                    || tail.starts_with("&apos;"),
                "found a bare ampersand at byte {index}"
            );
        }
    }

    #[test]
    fn utf16_encoding_has_le_bom_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xml");
        write_utf16_xml(&path, "<a>hi</a>").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xFE]);
        let utf16_units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&utf16_units).unwrap(), "<a>hi</a>");
    }

    #[test]
    fn task_registration_uses_a_unique_temporary_xml_path() {
        let first = task_xml_temp_path();
        let second = task_xml_temp_path();

        assert_ne!(
            first, second,
            "concurrent registrations need separate task XML files"
        );
        assert_eq!(first.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            first
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(
                    |name| name.starts_with("nucleos-daemon-task-") && name.ends_with(".xml")
                )
        );
    }

    #[test]
    fn o_xml_da_tarefa_e_escrito_por_inteiro() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.xml");
        let short_xml = "<a/>";

        write_utf16_xml(
            &path,
            "<Task><Description>conteudo deliberadamente muito comprido</Description></Task>",
        )
        .unwrap();
        write_utf16_xml(&path, short_xml).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            2 + 2 * short_xml.encode_utf16().count() as u64
        );
    }

    // `is_registered`/`register` touch the real Windows Task Scheduler — no fake backend, same
    // reasoning as the Credential Manager test in Chunk 4 Task 1. #[ignore]d for the same reason.
    #[test]
    #[cfg(windows)]
    #[ignore = "touches the real Windows Task Scheduler; run with --include-ignored on a desktop session"]
    fn ensure_registered_creates_a_real_task() {
        let exe = std::env::current_exe().unwrap();
        ensure_registered(&exe).unwrap();
        let runs = |path: &Path| {
            matches!(
                task_state(path),
                TaskPresence::Present(TaskState {
                    runs_this_exe: true,
                    ..
                })
            )
        };
        assert!(runs(&exe));
        // A task registered for a DIFFERENT executable must not satisfy the check, which is the
        // whole reason it reads the command rather than the name.
        assert!(!runs(Path::new(r"C:\nowhere\other.exe")));
        // best-effort cleanup; leaving a stray dev-created task behind is a known limitation, not
        // a correctness issue for this test
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .status();
    }

    #[test]
    fn a_launch_agent_runs_this_exe_at_login_from_its_working_directory() {
        let plist = launch_agent_plist("/opt/n/nucleos-core", "/opt/n");
        assert!(
            plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
            "{plist}"
        );
        for line in [
            "<key>Label</key><string>dev.nucleos.core</string>",
            "<array><string>/opt/n/nucleos-core</string></array>",
            "<key>WorkingDirectory</key><string>/opt/n</string>",
            "<key>RunAtLoad</key><true/>",
            "<key>AbandonProcessGroup</key><true/>",
        ] {
            assert!(plist.contains(line), "missing {line:?} in {plist}");
        }
    }

    #[test]
    fn a_launch_agent_escapes_xml_in_the_path() {
        let plist = launch_agent_plist("/opt/R&D <x>/nucleos-core", "/opt/R&D <x>");
        assert!(plist.contains("<string>/opt/R&amp;D &lt;x&gt;/nucleos-core</string>"));
        assert!(plist.contains("<string>/opt/R&amp;D &lt;x&gt;</string>"));
        assert!(!plist.contains("R&D"), "{plist}");
    }

    /// systemd splits `ExecStart=` on whitespace unless a word is quoted, and expands `%` specifiers
    /// and `$` variables, so an unquoted path with a space, a `%` or a `$` in it runs something else.
    #[test]
    fn a_user_unit_quotes_a_path_with_spaces_percent_and_dollar_and_keeps_the_default_kill_mode() {
        let unit = systemd_unit(
            "/home/me/my 100% $HOME/nucleos-core",
            "/home/me/my 100% $HOME",
        );
        for line in [
            r#"ExecStart="/home/me/my 100%% $$HOME/nucleos-core""#,
            "WorkingDirectory=/home/me/my 100%% $HOME",
            "Restart=on-failure",
            "RestartSec=60",
            "StartLimitBurst=3",
            "WantedBy=default.target",
        ] {
            assert!(
                unit.lines().any(|l| l == line),
                "missing {line:?} in {unit}"
            );
        }
        assert!(
            !unit.contains("KillMode="),
            "the default control-group kill mode must remain in effect: {unit}"
        );
        assert_eq!(systemd_quote(r#"a\b"c"#), r#""a\\b\"c""#);
    }

    /// The Desktop Entry spec quotes an `Exec=` argument and THEN applies the file's own string
    /// escapes, so one backslash in the path is four in the file.
    #[test]
    fn a_desktop_entry_quotes_the_exec_line() {
        let entry = xdg_desktop_entry("/home/me/$x \"y\" 5%/nucleos-core", "/home/me");
        for line in [
            "[Desktop Entry]",
            "Type=Application",
            r#"Exec="/home/me/\\$x \\"y\\" 5%%/nucleos-core""#,
            "Path=/home/me",
            "NoDisplay=true",
        ] {
            assert!(
                entry.lines().any(|l| l == line),
                "missing {line:?} in {entry}"
            );
        }
        assert_eq!(desktop_quote("a\\b`c"), r#""a\\\\b\\`c""#);
    }

    /// The Windows rule, kept: registered means "launches THIS executable", never "a file with that
    /// name exists".
    #[test]
    fn an_entry_is_ours_only_when_it_is_exactly_what_we_would_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nucleos-core.service");
        let ours = systemd_unit("/opt/n/nucleos-core", "/opt/n");
        assert!(!entry_is_current(&path, &ours));
        std::fs::write(&path, &ours).unwrap();
        assert!(entry_is_current(&path, &ours));
        let other = systemd_unit("/opt/elsewhere/nucleos-core", "/opt/elsewhere");
        assert!(!entry_is_current(&path, &other));
    }

    /// A line break would end the value early and let the rest of the path write lines of its own
    /// into the entry, such as an `ExecStartPre=`.
    #[test]
    fn a_path_with_a_line_break_cannot_become_an_autostart_entry() {
        assert_eq!(
            one_line(Path::new("/opt/my n/nucleos-core")).unwrap(),
            "/opt/my n/nucleos-core"
        );
        assert!(one_line(Path::new("/opt/n\nExecStartPre=/bin/evil")).is_err());
        assert!(one_line(Path::new("/opt/n\r/nucleos-core")).is_err());
    }

    /// Two Linux backends must never both be installed (review of P1-6): a daemon that once found
    /// no user bus writes an XDG entry, and if the systemd unit from an earlier start is still
    /// enabled both launch at login, so the second daemon marks the first's runs interrupted.
    #[test]
    fn the_other_linux_backends_entries_are_the_ones_to_remove() {
        let config = tempfile::tempdir().unwrap();
        let unit = config
            .path()
            .join("systemd")
            .join("user")
            .join("nucleos-core.service");
        let wants = config
            .path()
            .join("systemd")
            .join("user")
            .join("default.target.wants")
            .join("nucleos-core.service");
        let desktop = config.path().join("autostart").join("nucleos-core.desktop");
        assert!(
            other_backend_entries(config.path(), true).is_empty(),
            "nothing on disk, nothing to remove"
        );
        for path in [&unit, &wants, &desktop] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        assert_eq!(
            other_backend_entries(config.path(), true),
            vec![desktop.clone()]
        );
        assert_eq!(
            other_backend_entries(config.path(), false),
            vec![unit.clone(), wants.clone()]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes a real LaunchAgent for this user; run with --include-ignored on a Mac"]
    fn ensure_registered_writes_a_real_launch_agent() {
        let exe = std::env::current_exe().unwrap();
        ensure_registered(&exe).unwrap();
        assert!(is_registered(&exe));
        assert!(!is_registered(Path::new("/nowhere/other")));
        // Nothing was loaded into launchd, so removing the file is the whole undo.
        let _ = std::fs::remove_file(launch_agent_path().unwrap());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    #[ignore = "writes and enables a real autostart entry for this user; run with --include-ignored"]
    fn ensure_registered_enables_a_real_user_entry() {
        let exe = std::env::current_exe().unwrap();
        let systemd = has_user_systemd();
        let backend = if systemd {
            "systemd user unit"
        } else {
            "XDG autostart entry"
        };
        eprintln!("autostart backend under test: {backend}");
        ensure_registered(&exe).unwrap();
        assert!(is_registered(&exe));
        assert!(!is_registered(Path::new("/nowhere/other")));
        // Best-effort cleanup, as on Windows.
        let (path, _) = linux_entry(&exe, systemd).unwrap();
        if systemd {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", USER_UNIT])
                .status();
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_installed_daemon_works_from_its_own_directory() {
        for (exe, dir) in [
            (
                "C:/Users/me/AppData/Local/NucleOS/nucleos-core.exe",
                "C:/Users/me/AppData/Local/NucleOS",
            ),
            ("/usr/bin/nucleos-core", "/usr/bin"),
            (
                "/Applications/NucleOS.app/Contents/MacOS/nucleos-core",
                "/Applications/NucleOS.app/Contents/MacOS",
            ),
            (
                "C:/Projects/.cargo-target-x/debug/nucleos-core.exe",
                "C:/Projects/.cargo-target-x/debug",
            ),
            (
                "C:/Projects/.nucleos-run/nucleos-core.exe",
                "C:/Projects/.nucleos-run",
            ),
        ] {
            assert_eq!(
                working_directory_for(Path::new(exe)),
                Path::new(dir),
                "{exe}"
            );
        }
    }

    #[test]
    fn a_dev_build_still_works_from_the_repo_root() {
        for exe in [
            "C:/Projects/nucleos/core/target/debug/nucleos-core.exe",
            "C:/Projects/nucleos/target/release/nucleos-core.exe",
        ] {
            assert_eq!(
                working_directory_for(Path::new(exe)),
                Path::new("C:/Projects/nucleos"),
                "{exe}"
            );
        }
    }

    /// Registering again at every start used to overwrite the task with the login one. A boot task
    /// that points at a moved executable only has its command changed; it is never recreated, which
    /// would need the password nobody keeps.
    #[test]
    fn reregistering_keeps_at_boot_mode() {
        let moved_boot_task = TaskPresence::Present(TaskState {
            runs_this_exe: false,
            at_boot: true,
        });
        assert_eq!(registration_plan(moved_boot_task), Registration::RepointExe);
        let args = repoint_args(Path::new(r"C:\new place\nucleos-core.exe"));
        assert_eq!(args[0], "/Change");
        assert!(
            args.last()
                .unwrap()
                .contains(r#"--headless "C:\new place\nucleos-core.exe" --supervise"#),
            "{args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "/XML" | "/Create" | "/RP" | "/F")),
            "{args:?}"
        );

        let current_boot_task = TaskPresence::Present(TaskState {
            runs_this_exe: true,
            at_boot: true,
        });
        assert_eq!(registration_plan(current_boot_task), Registration::Keep);
        let moved_login_task = TaskPresence::Present(TaskState {
            runs_this_exe: false,
            at_boot: false,
        });
        assert_eq!(
            registration_plan(moved_login_task),
            Registration::CreateAtLogin
        );
        assert_eq!(
            registration_plan(TaskPresence::Absent),
            Registration::CreateAtLogin
        );
    }

    /// A transient failure to query must not be read as "no task": that would overwrite a boot task
    /// with the login one and lose the password stored with it.
    #[test]
    fn a_failed_query_keeps_the_boot_task() {
        assert_eq!(
            registration_plan(TaskPresence::QueryFailed),
            Registration::Keep
        );
        // Only a listing that worked and lacks our name proves the task is absent.
        assert!(task_is_absent(Some("\"\\Other task\",\"N/A\",\"Ready\"\n")));
        assert!(!task_is_absent(Some(
            "\"\\NucleOS Daemon\",\"At system start up\",\"Ready\"\n"
        )));
        assert!(!task_is_absent(None));
    }

    #[test]
    fn an_appimage_gets_no_boot_mode() {
        use std::ffi::OsStr;
        let on = capability_for(Platform::Linux, true);
        let appimage = without_appimage(on.clone(), Some(OsStr::new("/tmp/NucleOS.AppImage")));
        assert!(!appimage.at_boot && appimage.why_not.is_some());
        assert_eq!(without_appimage(on.clone(), None), on);
    }

    #[test]
    fn at_boot_xml_uses_a_boot_trigger() {
        let xml = at_boot_task_xml(Path::new(r"C:\Apps\nucleos-core.exe"), r"DESKTOP\Dev & Ops");
        assert!(xml.contains("<BootTrigger>"), "{xml}");
        assert!(!xml.contains("<LogonTrigger>"), "{xml}");
        assert!(xml.contains("<LogonType>Password</LogonType>"), "{xml}");
        assert!(!xml.contains("S4U"), "{xml}");
        assert!(
            xml.contains(r"<UserId>DESKTOP\Dev &amp; Ops</UserId>"),
            "{xml}"
        );
        assert!(
            xml.contains(r#"--headless "C:\Apps\nucleos-core.exe" --supervise"#),
            "{xml}"
        );
        assert!(xml_is_at_boot(&xml));
        // And the login task stays what it was.
        let login = task_xml(Path::new(r"C:\Apps\nucleos-core.exe"));
        assert!(login.contains("<LogonTrigger>") && !xml_is_at_boot(&login));
    }

    #[test]
    fn the_password_never_reaches_the_task_xml_or_the_logs() {
        let password = "hunter2-correct-horse";
        let exe = Path::new(r"C:\Apps\nucleos-core.exe");
        // The document has no password input, so none can be in it.
        assert!(!at_boot_task_xml(exe, r"PC\me").contains(password));

        // The password has to reach schtasks, as exactly one argument.
        let args = at_boot_create_args(Path::new(r"C:\t\task.xml"), r"PC\me", password);
        let at = args.iter().position(|arg| arg == "/RP").unwrap();
        assert_eq!(args[at + 1], password);
        assert_eq!(args.iter().filter(|arg| arg.contains(password)).count(), 1);

        // Whatever the tool says back is scrubbed before it can be shown or logged.
        let stderr = format!("ERROR: Invalid credentials for user. Password {password} rejected.");
        let stdout = format!("echo of {password}");
        let failure = schtasks_failure(&stderr, &stdout, password);
        let shown = failure.to_string();
        assert!(matches!(failure, SetModeError::Failed(_)));
        assert!(!shown.contains(password), "{shown}");
        assert!(shown.contains("Invalid credentials"), "{shown}");
        assert!(shown.contains("echo of ***"), "{shown}");
        // An empty password must not turn the scrub into "insert *** between every letter".
        assert_eq!(scrub("abc", ""), "abc");
    }

    #[test]
    fn each_platform_reports_its_capability() {
        let windows = capability_for(Platform::Windows, false);
        assert_eq!(
            windows,
            Capability {
                at_boot: true,
                needs_password: true,
                why_not: None
            }
        );

        let mac = capability_for(Platform::Macos, true);
        assert!(!mac.at_boot && !mac.needs_password);
        let why = mac.why_not.unwrap();
        assert!(
            why.contains("LaunchAgent") && why.contains("Keychain"),
            "{why}"
        );
        assert!(why.contains("automatic login"), "{why}");

        let linux = capability_for(Platform::Linux, true);
        assert!(linux.at_boot && !linux.needs_password && linux.why_not.is_none());
        let bare = capability_for(Platform::Linux, false);
        assert!(!bare.at_boot && bare.why_not.is_some());
    }

    #[test]
    fn linger_decides_the_linux_mode() {
        assert_eq!(mode_from_linger("Linger=yes\n"), Mode::AtBoot);
        assert_eq!(mode_from_linger("Linger=no\n"), Mode::AtLogin);
        assert_eq!(mode_from_linger(""), Mode::AtLogin);
        assert_eq!(linger_verb(Mode::AtBoot), "enable-linger");
        assert_eq!(linger_verb(Mode::AtLogin), "disable-linger");
    }

    #[test]
    fn an_appimage_mount_is_never_registered() {
        use std::ffi::OsStr;
        assert!(skip_registration(Some(OsStr::new("/tmp/NucleOS.AppImage"))));
        assert!(!skip_registration(Some(OsStr::new(""))));
        assert!(!skip_registration(None));
    }
}
