use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelsConfig {
    pub claude_model: String,
    pub codex_model: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig {
            claude_model: "claude-sonnet-5".to_string(),
            codex_model: "gpt-5.6-terra".to_string(),
        }
    }
}

pub fn load_models_config(path: &Path) -> std::io::Result<ModelsConfig> {
    if !path.exists() {
        return Ok(ModelsConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn discover_autopilot_config(
    project_root: &Path,
) -> std::io::Result<Option<serde_yaml::Value>> {
    let path = project_root.join(".ai").join("autopilot.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)?;
    let value = serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ScheduleRule {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RepoTrigger {
    pub name: String,
    pub branch: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct AutopilotRules {
    #[serde(default)]
    pub schedules: Vec<ScheduleRule>,
    #[serde(default)]
    pub repo_triggers: Vec<RepoTrigger>,
}

pub fn load_schedule_rules(project_root: &Path) -> std::io::Result<AutopilotRules> {
    let path = project_root.join(".ai").join("autopilot.yaml");
    if !path.exists() {
        return Ok(AutopilotRules::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        assert_eq!(load_models_config(&path).unwrap(), ModelsConfig::default());
    }

    #[test]
    fn parses_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();
        assert_eq!(config.claude_model, "claude-opus-4-8");
        assert_eq!(config.codex_model, "gpt-5.6-sol");
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(&path, "not: [valid, yaml for this struct").unwrap();
        assert!(load_models_config(&path).is_err());
    }

    #[test]
    fn discover_returns_none_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(discover_autopilot_config(dir.path()).unwrap(), None);
    }

    #[test]
    fn discover_parses_present_file_as_generic_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        // Deliberately a placeholder shape, not a real Autopilot schema commitment —
        // that schema is designed in the Autopilot plan, not here.
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "placeholder_field: placeholder_value\n",
        )
        .unwrap();

        let value = discover_autopilot_config(dir.path()).unwrap().unwrap();
        assert_eq!(
            value.get("placeholder_field").and_then(|v| v.as_str()),
            Some("placeholder_value")
        );
    }

    #[test]
    fn schedule_rules_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_schedule_rules(dir.path()).unwrap(),
            AutopilotRules::default()
        );
    }

    #[test]
    fn schedule_rules_parses_two_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: nightly-build\n\
             \x20\x20\x20\x20cron: \"0 2 * * *\"\n\
             \x20\x20\x20\x20prompt: \"run the nightly build\"\n\
             \x20\x20\x20\x20cwd: /repo\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert_eq!(rules.schedules.len(), 2);

        assert_eq!(rules.schedules[0].name, "nightly-build");
        assert_eq!(rules.schedules[0].cron, "0 2 * * *");
        assert_eq!(rules.schedules[0].prompt, "run the nightly build");
        assert_eq!(rules.schedules[0].cwd, Some("/repo".to_string()));

        assert_eq!(rules.schedules[1].name, "morning-report");
        assert_eq!(rules.schedules[1].cron, "0 8 * * *");
        assert_eq!(rules.schedules[1].prompt, "summarize overnight activity");
    }

    #[test]
    fn schedule_rules_entry_without_cwd_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert_eq!(rules.schedules[0].cwd, None);
    }

    #[test]
    fn schedule_rules_malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules: [not, valid, for this struct",
        )
        .unwrap();

        assert!(load_schedule_rules(dir.path()).is_err());
    }

    #[test]
    fn repo_triggers_parse_and_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "repo_triggers:\n\
             \x20\x20- name: review-main\n\
             \x20\x20\x20\x20branch: main\n\
             \x20\x20\x20\x20prompt: \"review new commits on main\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert!(rules.schedules.is_empty());
        assert_eq!(rules.repo_triggers.len(), 1);
        assert_eq!(rules.repo_triggers[0].name, "review-main");
        assert_eq!(rules.repo_triggers[0].branch, "main");
        assert_eq!(rules.repo_triggers[0].prompt, "review new commits on main");
    }
}
