use std::path::{Path, PathBuf};
use std::process::Command;

const TASK_NAME: &str = "NucleOS Daemon";

pub fn ensure_registered(exe_path: &Path) -> std::io::Result<()> {
    if is_registered() {
        return Ok(());
    }
    register(exe_path)
}

fn is_registered() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
        exe_path.display(),
        working_directory.display()
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
        assert!(is_registered());
        // best-effort cleanup; leaving a stray dev-created task behind is a known limitation, not
        // a correctness issue for this test
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .status();
    }
}
