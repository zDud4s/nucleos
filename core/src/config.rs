use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelsConfig {
    pub claude_model: String,
    pub codex_model: String,
    /// Absent keeps the established CLI path active, which lets local triage ship dark until an
    /// operator explicitly names a model that can keep message bodies on the machine.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_triage_model: Option<String>,
    /// Which agent CLI answers a run. Absent — or naming anything startup does not recognise — keeps
    /// the proven Claude path, so the second runner ships dark until an operator asks for it by
    /// name, the same posture `local_triage_model` gives local inference.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub primary_runner: Option<String>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig {
            claude_model: "claude-sonnet-5".to_string(),
            codex_model: "gpt-5.6-terra".to_string(),
            local_triage_model: None,
            primary_runner: None,
        }
    }
}

fn deserialize_optional_model<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty()))
}

pub fn load_models_config(path: &Path) -> std::io::Result<ModelsConfig> {
    if !path.exists() {
        return Ok(ModelsConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `.ai/email.yaml` (spec §3.4). Every field has a default, so a partial file is valid and an
/// absent one switches the pillar off in silence rather than blocking startup.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmailConfig {
    /// Opt-in. The pillar reads a real mailbox, so nothing about it starts by accident.
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub mailbox: String,
    pub poll_interval_secs: u64,
    /// Which triage classes are worth interrupting a person for.
    ///
    /// An EMPTY list is a valid value meaning "notify about nothing" — only an ABSENT key gives
    /// the `["urgent"]` default. The distinction carries the rollout's first week (spec §9), where
    /// the whole point is to watch the classifier without being paged by it; treating `[]` as
    /// absent would notify from day one and burn the calibration ramp.
    pub notify_classes: Vec<String>,
    /// Hour (UTC) the daily digest is emitted. Validated to `0..=21` so its two-hour window cannot
    /// straddle midnight and emit twice (§6.3).
    pub digest_hour_utc: u8,
    /// How long a classified message keeps its body (§7.2). Defaults to 14 rather than 0 because
    /// the pillar's first state after being switched on is always the calibration week, which is
    /// exactly when the bodies are needed. Capped at 30, where row pruning removes the row anyway.
    pub retain_bodies_days: u8,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 993,
            username: String::new(),
            mailbox: "INBOX".to_string(),
            poll_interval_secs: 300,
            notify_classes: vec!["urgent".to_string()],
            digest_hour_utc: 7,
            retain_bodies_days: 14,
        }
    }
}

impl EmailConfig {
    /// Clamps the two validated fields, warning about what it changed. Out-of-range values are
    /// corrected rather than fatal, for the same reason the whole file is: nothing in this config
    /// is worth refusing to start the daemon over.
    fn validated(mut self) -> Self {
        if self.digest_hour_utc > 21 {
            tracing::warn!(
                digest_hour_utc = self.digest_hour_utc,
                "email config: digest_hour_utc must be 0..=21 so the window cannot cross midnight; using 7"
            );
            self.digest_hour_utc = 7;
        }
        if self.retain_bodies_days > 30 {
            tracing::warn!(
                retain_bodies_days = self.retain_bodies_days,
                "email config: retain_bodies_days must be 0..=30; using 30"
            );
            self.retain_bodies_days = 30;
        }
        self
    }
}

/// Reads `.ai/email.yaml`. Absent or unreadable → defaults, with a warning; never an error, so a
/// typo in an optional pillar's config cannot stop the daemon from starting.
pub fn load_email_config(path: &Path) -> EmailConfig {
    if !path.exists() {
        return EmailConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<EmailConfig>(&text)) {
        Ok(Ok(config)) => config.validated(),
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be parsed; the pillar stays off");
            EmailConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be read; the pillar stays off");
            EmailConfig::default()
        }
    }
}

/// `deny_unknown_fields` on every rule type and on the file itself: without it a typo like
/// `schedule:` for `schedules:` parses cleanly into an empty ruleset, and all autonomy for that
/// project silently stops. That direction is fail-closed, which is precisely why nobody notices —
/// the contract this module advertises is "error on malformed YAML rather than guess", and a
/// misspelt key is malformed.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRule {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub cwd: Option<String>,
    /// The IANA zone the cron is read in — `Europe/Lisbon`, `America/New_York`.
    ///
    /// Absent means UTC, which is what every rule written before this field already meant, so no
    /// existing schedule moves by adding it. It exists because UTC is the one answer that is wrong
    /// twice a year for most of the world: `0 8 * * *` is 08:00 local in winter and 09:00 in
    /// summer, and the daily task that quietly starts an hour late for half the year is a worse
    /// failure than one that never runs, because nothing about it looks broken.
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoTrigger {
    pub name: String,
    pub branch: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutopilotRules {
    #[serde(default)]
    pub schedules: Vec<ScheduleRule>,
    #[serde(default)]
    pub repo_triggers: Vec<RepoTrigger>,
    #[serde(default)]
    pub gate_command: Option<String>,
}

pub fn load_schedule_rules(project_root: &Path) -> std::io::Result<AutopilotRules> {
    let path = project_root.join(".ai").join("autopilot.yaml");
    if !path.exists() {
        return Ok(AutopilotRules::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    // A file with no YAML document in it -- empty, or nothing but comments -- is a fourth state, and
    // it must land with "absent" rather than with "unreadable". serde_yaml returns EndOfStream here,
    // which would otherwise become `GateConfig::Unreadable` and report `gate errored` on every
    // completed run. The way an operator switches a gate off for an afternoon is to comment the
    // `gate_command:` line out; in this repository's own config that leaves comments only.
    if contents.trim().is_empty() {
        return Ok(AutopilotRules::default());
    }
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email_config_from(yaml: &str) -> EmailConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("email.yaml");
        std::fs::write(&path, yaml).unwrap();
        load_email_config(&path)
    }

    #[test]
    fn an_absent_email_config_leaves_the_pillar_off() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_email_config(&dir.path().join("email.yaml"));
        assert_eq!(config, EmailConfig::default());
        assert!(!config.enabled);
    }

    /// A typo in an optional pillar's config must not stop the daemon from starting.
    #[test]
    fn an_unparseable_email_config_leaves_the_pillar_off() {
        assert_eq!(
            email_config_from("enabled: [this is not a bool"),
            EmailConfig::default()
        );
    }

    #[test]
    fn a_partial_email_config_keeps_the_defaults_for_the_rest() {
        let config = email_config_from("enabled: true\nhost: imap.gmail.com\nusername: me@x.com\n");
        assert!(config.enabled);
        assert_eq!(config.host, "imap.gmail.com");
        assert_eq!(config.port, 993);
        assert_eq!(config.mailbox, "INBOX");
        assert_eq!(config.notify_classes, vec!["urgent".to_string()]);
        assert_eq!(config.retain_bodies_days, 14);
    }

    /// The distinction the rollout's first week depends on: an EMPTY list means "notify about
    /// nothing", and only an absent key means "urgent". Collapsing the two would page the user from
    /// day one and burn the calibration ramp.
    #[test]
    fn an_empty_notify_list_is_a_real_setting() {
        assert_eq!(
            email_config_from("notify_classes: []\n").notify_classes,
            Vec::<String>::new()
        );
        assert_eq!(
            email_config_from("enabled: true\n").notify_classes,
            vec!["urgent".to_string()]
        );
    }

    /// A window starting after 21:00 UTC would cross midnight and emit two digests (§6.3).
    #[test]
    fn a_digest_hour_that_would_cross_midnight_is_corrected() {
        assert_eq!(
            email_config_from("digest_hour_utc: 23\n").digest_hour_utc,
            7
        );
        assert_eq!(
            email_config_from("digest_hour_utc: 21\n").digest_hour_utc,
            21
        );
        assert_eq!(email_config_from("digest_hour_utc: 0\n").digest_hour_utc, 0);
    }

    /// Above 30 days the row itself is gone, so a larger value would keep nothing extra (§7.2).
    #[test]
    fn retention_is_capped_at_the_row_pruning_horizon() {
        assert_eq!(
            email_config_from("retain_bodies_days: 90\n").retain_bodies_days,
            30
        );
        assert_eq!(
            email_config_from("retain_bodies_days: 0\n").retain_bodies_days,
            0
        );
    }

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

    /// Local inference must remain an explicit opt-in: an absent or blank model keeps the proven
    /// CLI path active, while a real name is preserved for startup to construct the local runner.
    #[test]
    fn local_triage_model_is_optional_and_absent_means_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.claude_model, "claude-opus-4-8");
        assert_eq!(absent.local_triage_model, None);

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: qwen3.5:4b\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().local_triage_model,
            Some("qwen3.5:4b".to_string())
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: \"\"\n",
        )
        .unwrap();
        assert_eq!(load_models_config(&path).unwrap().local_triage_model, None);
    }

    /// The second runner ships dark, so what this key parses to is what decides whether a run is
    /// answered by the proven CLI or by one nobody asked for.
    ///
    /// The unrecognised case is the one worth a test: startup maps anything but `codex` back to the
    /// Claude runner, and it can only do that if loading SUCCEEDS and hands it the name. Were the
    /// deserializer to reject an unknown value instead, a typo in one key would take the whole
    /// daemon down — email, scheduler and all — rather than costing the operator the runner they
    /// misspelled.
    #[test]
    fn primary_runner_is_optional_and_an_unknown_name_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.codex_model, "gpt-5.6-sol");
        assert_eq!(
            absent.primary_runner, None,
            "an absent key must not opt a daemon into the second runner"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: codex\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().primary_runner,
            Some("codex".to_string()),
            "the one recognised name must survive parsing for startup to act on"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: gemini\n",
        )
        .unwrap();
        let unknown = load_models_config(&path)
            .expect("an unrecognised runner must cost the operator a warning, not the daemon");
        assert_eq!(
            unknown.primary_runner,
            Some("gemini".to_string()),
            "startup needs the name it did not recognise in order to warn about it"
        );
        assert_ne!(
            unknown.primary_runner.as_deref(),
            Some("codex"),
            "nothing but the exact name may reach the codex branch of the runner match"
        );
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(&path, "not: [valid, yaml for this struct").unwrap();
        assert!(load_models_config(&path).is_err());
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

    /// The state between "no file" and "broken file". serde_yaml reports a document-less stream as
    /// EndOfStream, which reads as malformed — so without this, commenting out the `gate_command:`
    /// line (the obvious way to switch a gate off) turns every completed worktree run into
    /// `gate errored`. Both spellings, because a comments-only file is the realistic one.
    #[test]
    fn schedule_rules_with_no_yaml_document_is_not_a_gate_rather_than_an_error() {
        for contents in ["", "   \n\n", "# just a comment\n# and another\n"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
            std::fs::write(dir.path().join(".ai").join("autopilot.yaml"), contents).unwrap();

            let rules = load_schedule_rules(dir.path())
                .unwrap_or_else(|e| panic!("{contents:?} must not be an error, got {e}"));
            assert_eq!(rules.gate_command, None);
            assert!(rules.schedules.is_empty());
            assert!(rules.repo_triggers.is_empty());
        }
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
