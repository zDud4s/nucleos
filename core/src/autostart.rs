use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Command;

#[cfg(windows)]
const TASK_NAME: &str = "NucleOS Daemon";

pub fn ensure_registered(exe_path: &Path) -> std::io::Result<()> {
    if is_registered(exe_path) {
        return Ok(());
    }
    register(exe_path)
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

/// Whether the registered task, if any, is one that actually launches THIS executable.
///
/// The name alone was the whole check, and the name is not a fact about the task: anything already
/// called "NucleOS Daemon" satisfied it forever, whatever it ran. The benign version of that is an
/// upgrade or a move leaving a task pointing at a path that no longer exists — silently, since
/// nothing looked. The unpleasant version is a same-user process pre-creating that name and owning
/// a logon-persistence slot this daemon would then refuse to correct.
#[cfg(windows)]
fn is_registered(exe_path: &Path) -> bool {
    let Ok(output) = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/XML", "ONE"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    // schtasks emits UTF-16LE here; comparing bytes would never match. Lossy is fine — a path that
    // does not survive the round trip is not one we would match anyway.
    let xml: String = output
        .stdout
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>()
        .iter()
        .map(|unit| char::from_u32(u32::from(*unit)).unwrap_or('\u{fffd}'))
        .collect();

    xml.to_lowercase()
        .contains(&xml_escape(&exe_path.to_string_lossy()).to_lowercase())
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

/// The dev build layout is `<repo_root>/core/target/debug/nucleos-core.exe` — the repo root is
/// four directory levels up from the exe file (debug -> target -> core -> repo root). Falls back
/// to the exe's own directory if the path is shallower than that (e.g. a future packaged layout),
/// which is still a reasonable place to look for `.ai/nucleos-models.yaml` (Chunk 5 Task 1) even
/// if not exactly right for every possible layout — better than omitting `<WorkingDirectory>`
/// altogether, which Task Scheduler does NOT default to the exe's own directory on its own.
fn working_directory_for(exe_path: &Path) -> PathBuf {
    exe_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .or_else(|| exe_path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(any(windows, test))]
fn task_xml(exe_path: &Path) -> String {
    let working_directory = working_directory_for(exe_path);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
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
        assert!(is_registered(&exe));
        // A task registered for a DIFFERENT executable must not satisfy the check, which is the
        // whole reason it reads the command rather than the name.
        assert!(!is_registered(Path::new(r"C:\nowhere\other.exe")));
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
}
