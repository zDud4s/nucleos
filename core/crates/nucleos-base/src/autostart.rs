use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Command;

#[cfg(any(windows, test))]
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
    match registration_plan(task_state(exe_path)) {
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

pub fn capability() -> Capability {
    let platform = Platform::current();
    capability_for(
        platform,
        platform == Platform::Linux && user_systemd_present(),
    )
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

/// PURE: what `ensure_registered` does about the task it found.
///
/// A new registration used to overwrite the task with the login one, which would silently undo
/// [`Mode::AtBoot`] on the next start. A boot task cannot be recreated here, because that needs
/// the password NucleOS never keeps, so one that points at a moved executable only has its command
/// changed and keeps its trigger and its credentials.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn registration_plan(task: Option<TaskState>) -> Registration {
    match task {
        Some(TaskState {
            runs_this_exe: true,
            ..
        }) => Registration::Keep,
        Some(TaskState { at_boot: true, .. }) => Registration::RepointExe,
        _ => Registration::CreateAtLogin,
    }
}

/// PURE: whether a task's XML has a boot trigger.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn xml_is_at_boot(xml: &str) -> bool {
    xml.contains("<BootTrigger>")
}

/// PURE: `schtasks` arguments that replace only the command of the existing task.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn repoint_args(exe_path: &Path) -> Vec<String> {
    vec![
        "/Change".to_owned(),
        "/TN".to_owned(),
        TASK_NAME.to_owned(),
        "/TR".to_owned(),
        format!("\"{}\"", exe_path.to_string_lossy()),
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

/// The registered task's XML, if there is one.
#[cfg(windows)]
fn query_task_xml() -> Option<String> {
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/XML", "ONE"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // schtasks emits UTF-16LE here; comparing bytes would never match. Lossy is fine — a path that
    // does not survive the round trip is not one we would match anyway.
    Some(
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
fn task_state(exe_path: &Path) -> Option<TaskState> {
    let xml = query_task_xml()?;
    Some(TaskState {
        runs_this_exe: xml
            .to_lowercase()
            .contains(&xml_escape(&exe_path.to_string_lossy()).to_lowercase()),
        at_boot: xml_is_at_boot(&xml),
    })
}

/// The mode the machine is in now.
#[cfg(windows)]
pub fn current_mode() -> Mode {
    match query_task_xml() {
        Some(xml) if xml_is_at_boot(&xml) => Mode::AtBoot,
        _ => Mode::AtLogin,
    }
}

#[cfg(windows)]
fn windows_user() -> std::io::Result<String> {
    let name = std::env::var("USERNAME")
        .map_err(|_| std::io::Error::other("no USERNAME to run the task as"))?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
        _ => name,
    })
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
            let failed =
                |error: std::io::Error| SetModeError::Failed(scrub(&error.to_string(), password));
            let user = windows_user().map_err(failed)?;
            let xml_path = std::env::temp_dir().join("nucleos-daemon-task.xml");
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
            let said = format!(
                "{} {}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            Err(SetModeError::Failed(scrub(&said, password)))
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

#[cfg(windows)]
fn register(exe_path: &Path) -> std::io::Result<()> {
    let xml = task_xml(exe_path);
    let xml_path = std::env::temp_dir().join("nucleos-daemon-task.xml");
    write_utf16_xml(&xml_path, &xml)?;
    let status = Command::new("schtasks")
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&xml_path)
        .arg("/F")
        .status()?;
    let _ = std::fs::remove_file(&xml_path);
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("schtasks /create failed"))
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

#[cfg(any(windows, test))]
fn task_xml(exe_path: &Path) -> String {
    task_document(
        exe_path,
        "<LogonTrigger>\n      <Enabled>true</Enabled>\n    </LogonTrigger>",
        "<LogonType>InteractiveToken</LogonType>",
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
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
      <WorkingDirectory>{}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>"#,
        xml_escape(&exe_path.to_string_lossy()),
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
</dict>
</plist>
"#,
        xml_escape(exe),
        xml_escape(workdir)
    )
}

/// PURE: the systemd user unit. The start limit mirrors the Windows task's `RestartOnFailure`
/// (three restarts, one minute apart).
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

    #[cfg(windows)]
    #[test]
    fn task_xml_embeds_the_exe_path_and_repo_root_working_directory() {
        let xml = task_xml(Path::new(
            r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe",
        ));
        assert!(xml.contains(r"C:\Projects\nucleos\core\target\debug\nucleos-core.exe"));
        assert!(xml.contains("<RestartOnFailure>"));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains(r"<WorkingDirectory>C:\Projects\nucleos</WorkingDirectory>"));
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
            !xml.contains("Ops\\nucleos\\core\\target\\debug\\nucleos-core.exe</Command>")
                || xml.contains("&amp;"),
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
        let runs = |path: &Path| task_state(path).is_some_and(|state| state.runs_this_exe);
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
    fn a_user_unit_quotes_a_path_with_spaces_percent_and_dollar() {
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
        let moved_boot_task = Some(TaskState {
            runs_this_exe: false,
            at_boot: true,
        });
        assert_eq!(registration_plan(moved_boot_task), Registration::RepointExe);
        let args = repoint_args(Path::new(r"C:\new place\nucleos-core.exe"));
        assert_eq!(args[0], "/Change");
        assert_eq!(args.last().unwrap(), r#""C:\new place\nucleos-core.exe""#);
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "/XML" | "/Create" | "/RP" | "/F")),
            "{args:?}"
        );

        let current_boot_task = Some(TaskState {
            runs_this_exe: true,
            at_boot: true,
        });
        assert_eq!(registration_plan(current_boot_task), Registration::Keep);
        let moved_login_task = Some(TaskState {
            runs_this_exe: false,
            at_boot: false,
        });
        assert_eq!(
            registration_plan(moved_login_task),
            Registration::CreateAtLogin
        );
        assert_eq!(registration_plan(None), Registration::CreateAtLogin);
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
            xml.contains(r"<Command>C:\Apps\nucleos-core.exe</Command>"),
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
        let said = format!("ERROR: Invalid credentials for user. Password {password} rejected.");
        let shown = SetModeError::Failed(scrub(&said, password)).to_string();
        assert!(!shown.contains(password), "{shown}");
        assert!(shown.contains("Invalid credentials"), "{shown}");
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
