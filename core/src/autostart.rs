use std::path::{Path, PathBuf};
use std::process::Command;

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

fn write_utf16_xml(path: &Path, xml: &str) -> std::io::Result<()> {
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // UTF-16LE BOM
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // `is_registered`/`register` touch the real Windows Task Scheduler — no fake backend, same
    // reasoning as the Credential Manager test in Chunk 4 Task 1. #[ignore]d for the same reason.
    #[test]
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
}
