//! Brings the settings of an already wired project up to date when the daemon starts. Without it a
//! hook added to `wire_hook` reaches a project only when someone happens to press a door that wires
//! it, and until then the project silently goes without.

use std::path::Path;

use super::{
    HOOK_SCRIPT, HOOK_SOURCE, classifier_hook_is_wired, wire_classifier_hook,
    wire_classifier_hook_keeping_script,
};

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

/// Brings every project this daemon has wired up to the current hook: the script and the timeouts.
///
/// Run at boot. A project wired before a change keeps its old script and its old `timeout` until
/// something rewrites them, and a project nobody opens again would keep a hook the CLI cuts off at
/// an hour while the daemon waits a month. Only projects `classifier_hook_is_wired` already
/// recognises are touched, so a project that never had the hook, or switched hooks off, gains
/// nothing. Returns how many were rewritten; one that cannot be is logged and skipped.
pub async fn rewire_wired_projects(pool: &sqlx::SqlitePool) -> sqlx::Result<usize> {
    let roots = sqlx::query_scalar::<_, String>(
        "SELECT project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let rewired = tokio::task::spawn_blocking(move || {
        let mut rewired = 0;
        for root in roots {
            let dir = Path::new(&root);
            let behind =
                classifier_hook_timeout_is_short(dir) || classifier_hook_script_is_stale(dir);
            if !classifier_hook_is_wired(dir) || !behind {
                continue;
            }
            match wire_classifier_hook(dir) {
                Ok(()) => rewired += 1,
                Err(error) => tracing::warn!("could not rewire the hook in {root}: {error}"),
            }
        }
        rewired
    })
    .await
    .unwrap_or(0);
    Ok(rewired)
}

/// Whether `dir` was wired before the current hook timeout, so it needs wiring again. A settings
/// file this cannot read counts as no: `wire_hook` would refuse it anyway. Asking first is what
/// keeps a current folder untouched: rewriting the script while a hook of another conversation is
/// reading it would break that hook, and a project tracking these files would show a diff.
pub fn classifier_hook_timeout_is_short(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(".claude").join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .is_some_and(|settings| super::hook_timeout::a_timeout_is_short(&settings))
}

/// Whether the script in `dir` is not the one this daemon ships. Only boot asks this, when no CLI
/// of this daemon is reading the script; a turn asks only about the timeout, since a worktree may
/// check out a copy of its own and that would be rewritten on every turn.
fn classifier_hook_script_is_stale(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(HOOK_SCRIPT)).map_or(true, |script| script != HOOK_SOURCE)
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
