//! Brings the settings of an already wired project up to date when the daemon starts. Without it a
//! hook added to `wire_hook` reaches a project only when someone happens to press a door that wires
//! it, and until then the project silently goes without.

use std::path::Path;

use super::{HOOK_SCRIPT, classifier_hook_is_wired, wire_classifier_hook_keeping_script};

/// Re-wires `dir` if its own `settings.json` already names this daemon's hook, and says whether it
/// did. Never the door that first wires a project: that stays somebody's decision, as does a project
/// that switched hooks off or wired them only in its private `settings.local.json`.
pub fn refresh_wired_hooks(dir: &Path) -> Result<bool, String> {
    let settings = dir.join(".claude").join("settings.json");
    let names_our_hook =
        std::fs::read_to_string(settings).is_ok_and(|text| text.contains(HOOK_SCRIPT));
    if !names_our_hook || !classifier_hook_is_wired(dir) {
        return Ok(false);
    }
    wire_classifier_hook_keeping_script(dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::{stop_timer, wire_classifier_hook};
    use super::*;

    fn settings_path(dir: &Path) -> std::path::PathBuf {
        dir.join(".claude/settings.json")
    }

    fn timers_in(dir: &Path) -> usize {
        let text = std::fs::read_to_string(settings_path(dir)).unwrap();
        let settings: Value = serde_json::from_str(&text).unwrap();
        settings["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry.to_string().contains(stop_timer::SCRIPT))
            .count()
    }

    fn wired_before_the_timer(dir: &Path) {
        wire_classifier_hook(dir).unwrap();
        let text = std::fs::read_to_string(settings_path(dir)).unwrap();
        let mut settings: Value = serde_json::from_str(&text).unwrap();
        settings["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| !entry.to_string().contains(stop_timer::SCRIPT));
        std::fs::write(settings_path(dir), settings.to_string()).unwrap();
        std::fs::remove_dir_all(dir.join(stop_timer::SCRIPT).parent().unwrap()).unwrap();
    }

    #[test]
    fn a_wired_project_gets_the_entries_it_lacks() {
        let root = tempfile::TempDir::new().unwrap();
        wired_before_the_timer(root.path());
        assert_eq!(timers_in(root.path()), 0);

        assert_eq!(refresh_wired_hooks(root.path()), Ok(true));

        assert_eq!(timers_in(root.path()), 1);
        assert!(root.path().join(stop_timer::SCRIPT).is_file());
    }

    #[test]
    fn a_project_that_never_wired_the_hook_is_not_wired_by_a_restart() {
        let root = tempfile::TempDir::new().unwrap();
        assert_eq!(refresh_wired_hooks(root.path()), Ok(false));
        assert!(!root.path().join(".claude").exists());

        let own = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"mine"}]}]}}"#;
        std::fs::create_dir_all(root.path().join(".claude")).unwrap();
        std::fs::write(settings_path(root.path()), own).unwrap();
        assert_eq!(refresh_wired_hooks(root.path()), Ok(false));
        assert_eq!(
            std::fs::read_to_string(settings_path(root.path())).unwrap(),
            own
        );
    }

    #[test]
    fn switched_off_hooks_stay_off_and_their_settings_untouched() {
        let root = tempfile::TempDir::new().unwrap();
        wired_before_the_timer(root.path());
        let path = settings_path(root.path());
        let mut settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        settings["disableAllHooks"] = Value::Bool(true);
        let body = settings.to_string();
        std::fs::write(&path, &body).unwrap();

        assert_eq!(refresh_wired_hooks(root.path()), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn a_project_wired_only_in_its_local_settings_gets_no_settings_file() {
        let root = tempfile::TempDir::new().unwrap();
        wire_classifier_hook(root.path()).unwrap();
        let claude = root.path().join(".claude");
        std::fs::rename(
            settings_path(root.path()),
            claude.join("settings.local.json"),
        )
        .unwrap();

        assert_eq!(refresh_wired_hooks(root.path()), Ok(false));
        assert!(!settings_path(root.path()).exists());
    }

    #[test]
    fn the_projects_own_copy_of_the_hook_script_is_kept() {
        let root = tempfile::TempDir::new().unwrap();
        wired_before_the_timer(root.path());
        std::fs::write(root.path().join(HOOK_SCRIPT), "# the project's own\n").unwrap();

        assert_eq!(refresh_wired_hooks(root.path()), Ok(true));

        assert_eq!(
            std::fs::read_to_string(root.path().join(HOOK_SCRIPT)).unwrap(),
            "# the project's own\n"
        );
    }

    /// Every daemon start passes here, and a running session watches this file.
    #[test]
    fn settings_already_up_to_date_are_not_rewritten() {
        let root = tempfile::TempDir::new().unwrap();
        wire_classifier_hook(root.path()).unwrap();
        let path = settings_path(root.path());
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions.clone()).unwrap();

        let refreshed = refresh_wired_hooks(root.path());

        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(refreshed, Ok(true));
    }
}
