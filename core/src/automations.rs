//! Project automations: owner-written rules of the shape "when EVENT and CONDITIONS, do ACTION".
//!
//! A closed catalog: every event, condition and action is a typed variant, and nothing in the document is a
//! command, a script or a free expression. The document is a project settings document (`automations`) that only
//! the owner writes; a rule never grants an agent anything. Its actions run through the daemon's own machinery
//! (the git queue for `push`), never through an agent.

pub mod dispatch;
pub mod firings;

pub use dispatch::{Event, automation_enabled, dispatch};

use serde::{Deserialize, Serialize};

use crate::execution::vcs::{Branch, Remote};

/// The longest rule name: it is a key in the firings log and in the feed.
pub const MAX_RULE_NAME: usize = 64;

/// The project's automations. A missing document reads as [`Automations::default`]: the spec's two preset rules,
/// both switched off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Automations {
    pub rules: Vec<AutomationRule>,
}

impl Default for Automations {
    fn default() -> Self {
        Self {
            rules: preset_rules(),
        }
    }
}

/// The two rules the spec describes, shipped switched off so the owner only has to flip a switch.
pub fn preset_rules() -> Vec<AutomationRule> {
    vec![
        AutomationRule {
            name: "push-when-green".to_string(),
            enabled: false,
            on: Trigger::PublishedGreen,
            when: None,
            action: Action::Push {
                remote: "origin".to_string(),
            },
        },
        AutomationRule {
            name: "fix-red-ci".to_string(),
            enabled: false,
            on: Trigger::CiFinished {
                workflow: "gates".to_string(),
            },
            when: Some(Conditions {
                branch: None,
                conclusion: Some(CiConclusion::Failure),
                pushed_by: Some(PushedBy::Automation),
            }),
            action: Action::OpenIncident { auto_fix: true },
        },
    ]
}

/// One rule: a name, a switch, the event it answers, optional conditions, and the one action it takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutomationRule {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    pub on: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Conditions>,
    #[serde(rename = "do")]
    pub action: Action,
}

/// The events a rule can answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// The published branch reached a green sha: `promoted` with a release branch, `local_green` without one.
    PublishedGreen,
    /// A GitHub Actions workflow run finished.
    CiFinished { workflow: String },
}

/// Conditions over the event; every condition given must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<CiConclusion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_by: Option<PushedBy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiConclusion {
    Success,
    Failure,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushedBy {
    Automation,
}

/// The actions a rule can take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Fast-forward `remote`'s branch to the green sha, through the git queue.
    Push { remote: String },
    /// Open the branch's `ci-red` incident; `auto_fix` lets the incident launcher start a fix.
    OpenIncident { auto_fix: bool },
}

/// The enabled rules of the project's snapshot; the error is the reader's (an unreadable document).
pub fn enabled_rules(
    project_id: &str,
) -> Result<Vec<AutomationRule>, crate::project::rules::DocumentUnreadable> {
    Ok(crate::project::rules::automations(project_id)?
        .rules
        .into_iter()
        .filter(|rule| rule.enabled)
        .collect())
}

impl Automations {
    /// The rule called `name`, if any.
    pub fn rule(&self, name: &str) -> Option<&AutomationRule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    /// Refuses the whole document with a sentence naming the broken rule.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for rule in &self.rules {
            check_name(&rule.name)?;
            if seen.contains(&rule.name.as_str()) {
                return Err(format!(
                    "the automation name `{}` is used twice; names must be unique",
                    rule.name
                ));
            }
            seen.push(&rule.name);
            check_rule(rule)?;
        }
        // One push rule per branch, switched off or not: switching one on must not create an invalid state.
        let pushes: Vec<&AutomationRule> = self
            .rules
            .iter()
            .filter(|rule| matches!(rule.action, Action::Push { .. }))
            .collect();
        for (index, first) in pushes.iter().enumerate() {
            for second in &pushes[index + 1..] {
                let (a, b) = (branch_of(first), branch_of(second));
                if a.is_none() || b.is_none() || a == b {
                    return Err(format!(
                        "the automations `{}` and `{}` both push the same branch; keep one push rule per branch",
                        first.name, second.name
                    ));
                }
            }
        }
        Ok(())
    }
}

fn branch_of(rule: &AutomationRule) -> Option<&str> {
    rule.when.as_ref().and_then(|when| when.branch.as_deref())
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("an automation has an empty name".to_string());
    }
    if name.chars().count() > MAX_RULE_NAME {
        return Err(format!(
            "the automation name `{name}` is longer than {MAX_RULE_NAME} characters"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(format!(
            "the automation name `{name}` may only hold letters, digits, `_`, `.` and `-`"
        ));
    }
    Ok(())
}

fn check_rule(rule: &AutomationRule) -> Result<(), String> {
    let name = &rule.name;
    let on_ci = match &rule.on {
        Trigger::PublishedGreen => false,
        Trigger::CiFinished { workflow } => {
            if workflow.trim().is_empty() {
                return Err(format!(
                    "the automation `{name}` answers ci_finished with an empty workflow"
                ));
            }
            true
        }
    };
    match &rule.action {
        Action::Push { remote } => {
            if on_ci {
                return Err(format!(
                    "the automation `{name}` pushes on ci_finished; push only answers published_green"
                ));
            }
            Remote::new(remote)
                .map_err(|why| format!("the automation `{name}` names a bad remote: {why}"))?;
        }
        Action::OpenIncident { .. } => {
            if !on_ci {
                return Err(format!(
                    "the automation `{name}` opens an incident on published_green; open_incident only answers ci_finished"
                ));
            }
        }
    }
    if let Some(when) = &rule.when {
        if !on_ci && (when.conclusion.is_some() || when.pushed_by.is_some()) {
            return Err(format!(
                "the automation `{name}` sets conclusion or pushed_by, which only ci_finished has"
            ));
        }
        if let Some(branch) = &when.branch {
            Branch::new(branch)
                .map_err(|why| format!("the automation `{name}` names a bad branch: {why}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(name: &str, branch: Option<&str>, enabled: bool) -> AutomationRule {
        AutomationRule {
            name: name.to_string(),
            enabled,
            on: Trigger::PublishedGreen,
            when: branch.map(|branch| Conditions {
                branch: Some(branch.to_string()),
                ..Conditions::default()
            }),
            action: Action::Push {
                remote: "origin".to_string(),
            },
        }
    }

    fn doc(rules: Vec<AutomationRule>) -> Automations {
        Automations { rules }
    }

    #[test]
    fn enabled_rules_returns_only_the_enabled_ones() {
        let yaml = "rules:\n  - name: on-rule\n    enabled: true\n    on: {event: published_green}\n    do: {action: push, remote: origin}\n  - name: off-rule\n    enabled: false\n    on: {event: ci_finished, workflow: gates}\n    do: {action: open_incident, auto_fix: false}\n";
        crate::project::rules::testing::publish_automations("enabled-rules-ok", yaml);
        let rules = enabled_rules("enabled-rules-ok").expect("a readable document");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "on-rule");
    }

    #[test]
    fn enabled_rules_of_an_unreadable_document_is_an_error() {
        crate::project::rules::testing::publish_unreadable(
            "enabled-rules-bad",
            "automations",
            "broken on purpose",
        );
        assert!(enabled_rules("enabled-rules-bad").is_err());
    }

    #[test]
    fn the_default_document_has_the_two_preset_rules_switched_off_and_is_valid() {
        let default = Automations::default();
        assert_eq!(default.rules.len(), 2);
        let push = &default.rules[0];
        assert_eq!(push.name, "push-when-green");
        assert!(!push.enabled);
        assert_eq!(push.on, Trigger::PublishedGreen);
        assert_eq!(push.when, None);
        assert_eq!(
            push.action,
            Action::Push {
                remote: "origin".to_string()
            }
        );
        let fix = &default.rules[1];
        assert_eq!(fix.name, "fix-red-ci");
        assert!(!fix.enabled);
        assert_eq!(
            fix.on,
            Trigger::CiFinished {
                workflow: "gates".to_string()
            }
        );
        assert_eq!(
            fix.when,
            Some(Conditions {
                branch: None,
                conclusion: Some(CiConclusion::Failure),
                pushed_by: Some(PushedBy::Automation),
            })
        );
        assert_eq!(fix.action, Action::OpenIncident { auto_fix: true });
        assert_eq!(default.validate(), Ok(()));
        assert!(default.rules.iter().all(|rule| !rule.enabled));

        // A stored document without the key keeps the presets; an emptied one stays empty.
        let omitted: Automations = serde_json::from_str("{}").unwrap();
        assert_eq!(omitted, Automations::default());
        let emptied: Automations = serde_json::from_str(r#"{"rules": []}"#).unwrap();
        assert!(emptied.rules.is_empty());
    }

    #[test]
    fn a_push_rule_parses_from_the_spec_yaml() {
        let yaml = "rules:\n  - name: push-quando-verde\n    enabled: false\n    on:\n      event: published_green\n    do:\n      action: push\n      remote: origin\n";
        let parsed: Automations = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed, doc(vec![push("push-quando-verde", None, false)]));
        assert_eq!(parsed.validate(), Ok(()));
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let error = serde_json::from_str::<Automations>(r#"{"rules":[{"nme":"x"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn two_rules_with_the_same_name_are_refused() {
        let error = doc(vec![
            push("same", Some("a"), true),
            push("same", Some("b"), true),
        ])
        .validate()
        .unwrap_err();
        assert!(error.contains("same"), "{error}");
        assert!(error.contains("twice"), "{error}");
    }

    #[test]
    fn two_push_rules_for_the_same_branch_are_refused() {
        assert!(
            doc(vec![
                push("one", Some("master"), true),
                push("two", Some("master"), true)
            ])
            .validate()
            .is_err()
        );
        assert!(
            doc(vec![
                push("one", None, true),
                push("two", Some("master"), true)
            ])
            .validate()
            .is_err()
        );
        assert_eq!(
            doc(vec![
                push("one", Some("a"), true),
                push("two", Some("b"), true)
            ])
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn a_disabled_second_push_rule_still_conflicts() {
        let error = doc(vec![push("one", None, true), push("two", None, false)])
            .validate()
            .unwrap_err();
        assert!(error.contains("two"), "{error}");
    }

    #[test]
    fn a_push_on_ci_finished_is_refused_and_so_is_open_incident_on_published_green() {
        let mut ci_push = push("ci-push", None, true);
        ci_push.on = Trigger::CiFinished {
            workflow: "gates".to_string(),
        };
        assert!(
            doc(vec![ci_push])
                .validate()
                .unwrap_err()
                .contains("ci-push")
        );

        let mut green_incident = push("green-incident", None, true);
        green_incident.action = Action::OpenIncident { auto_fix: false };
        assert!(
            doc(vec![green_incident])
                .validate()
                .unwrap_err()
                .contains("green-incident")
        );
    }

    #[test]
    fn a_when_conclusion_on_published_green_is_refused() {
        let mut rule = push("green", None, true);
        rule.when = Some(Conditions {
            conclusion: Some(CiConclusion::Failure),
            ..Conditions::default()
        });
        assert!(doc(vec![rule]).validate().is_err());
    }

    #[test]
    fn a_remote_that_starts_with_a_dash_is_refused() {
        let mut rule = push("dash", None, true);
        rule.action = Action::Push {
            remote: "--upload-pack=x".to_string(),
        };
        assert!(doc(vec![rule]).validate().unwrap_err().contains("remote"));
    }

    #[test]
    fn a_rule_name_with_a_space_or_over_64_chars_is_refused() {
        assert!(doc(vec![push("has space", None, true)]).validate().is_err());
        assert!(
            doc(vec![push(&"x".repeat(65), None, true)])
                .validate()
                .is_err()
        );
        assert_eq!(
            doc(vec![push(&"x".repeat(64), None, true)]).validate(),
            Ok(())
        );
    }
}
