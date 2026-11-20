//! Capability probes of the router: whether an engine can take a run of a given shape, answered
//! from what its adapter declares (spec `2026-11-18-motor-por-contrato-design.md` §3.3, §6.2), never
//! from the engine's name. `codex_refusal` stays in the parent as the named exception until the
//! 2026-11-03 gate lands (§10 phase 3); the tests here pin that this probe agrees with it.

use super::{RunShape, RunnerKind};
use crate::engines::adapters::{capabilities_of, execution_of};
use crate::execution::runner::{Permission, ToolPolicy};

/// Why the engine would refuse a run of this shape, in a sentence; `None` when it can take it, and
/// also `None` for an engine with no adapter (the caller decides what an unregistered one may do).
/// Asks the declared capabilities: a permission rung outside `permission_modes`, a classifier-governed
/// run without a wired `PreToolUse` hook, and a tool policy the CLI cannot enforce itself.
pub(super) fn shape_refusal(id: &str, shape: RunShape) -> Option<String> {
    let capabilities = capabilities_of(id)?;
    if !capabilities.permission_modes.contains(&shape.permission) {
        if shape.permission == Permission::Plan {
            return Some(format!(
                "{id} has no plan mode, so a plan-only run cannot be pinned to it"
            ));
        }
        return Some(format!(
            "{id} cannot honour Permission::{:?}",
            shape.permission
        ));
    }
    if shape.classifier_governs_tools {
        let wired = execution_of(id)?.wire_pretool("probe");
        if let Err(refusal) = wired {
            return Some(format!(
                "{id}: {refusal}, so a classifier-governed run cannot be pinned to it"
            ));
        }
    }
    if shape.tool_policy != ToolPolicy::Unrestricted
        && !(capabilities.tool_policy_none && capabilities.denied_tools)
    {
        return Some(format!(
            "{id} cannot honour ToolPolicy::{:?}",
            shape.tool_policy
        ));
    }
    None
}

/// The model the models file names for an engine, `None` for an engine it names none for.
pub(super) fn model_of<'a>(models: &'a crate::config::ModelsConfig, id: &str) -> Option<&'a str> {
    match id {
        crate::engines::CLAUDE => Some(&models.claude_model),
        crate::engines::CODEX => Some(&models.codex_model),
        _ => None,
    }
}

/// The engine a picker row names; a row naming none belongs to the table's default engine.
pub(super) fn choice_runner(choice: &crate::config::AssistantChoice) -> &str {
    choice
        .runner
        .as_deref()
        .unwrap_or_else(|| crate::engines::runner_id_in(crate::engines::TABLE, ""))
}

/// Every engine with an adapter, as router kinds, in registry order.
pub(super) fn registered_kinds() -> Vec<RunnerKind> {
    crate::engines::adapters::registered_ids()
        .into_iter()
        .filter_map(RunnerKind::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::route_advice::codex_refusal;

    fn shape() -> RunShape {
        RunShape {
            permission: Permission::Default,
            classifier_governs_tools: false,
            tool_policy: ToolPolicy::Unrestricted,
            resume: false,
        }
    }

    /// Every shape a run can have: the cross product of the three inputs the probe reads.
    fn every_shape() -> Vec<RunShape> {
        let permissions = [
            Permission::Default,
            Permission::AcceptEdits,
            Permission::Plan,
            Permission::Bypass,
        ];
        let policies = [
            ToolPolicy::Unrestricted,
            ToolPolicy::McpOnly,
            ToolPolicy::None,
        ];
        let mut shapes = Vec::new();
        for permission in permissions {
            for tool_policy in policies {
                for classifier_governs_tools in [false, true] {
                    shapes.push(RunShape {
                        permission,
                        classifier_governs_tools,
                        tool_policy,
                        resume: false,
                    });
                }
            }
        }
        shapes
    }

    #[test]
    fn claude_takes_every_shape_as_before() {
        for shape in every_shape() {
            assert_eq!(shape_refusal("claude", shape), None, "{shape:?}");
        }
    }

    #[test]
    fn codex_refuses_exactly_the_shapes_codex_refusal_does() {
        for shape in every_shape() {
            assert_eq!(
                shape_refusal("codex", shape).is_some(),
                codex_refusal(shape).is_some(),
                "{shape:?}"
            );
        }
        assert_eq!(shape_refusal("codex", shape()), None);
    }

    #[test]
    fn codex_refusals_name_the_same_cause_as_codex_refusal() {
        let plan = RunShape {
            permission: Permission::Plan,
            ..shape()
        };
        assert_eq!(shape_refusal("codex", plan), codex_refusal(plan));
        let bypass = RunShape {
            permission: Permission::Bypass,
            ..shape()
        };
        assert!(
            shape_refusal("codex", bypass)
                .unwrap()
                .contains("cannot honour Permission::Bypass")
        );
        let governed = RunShape {
            classifier_governs_tools: true,
            ..shape()
        };
        assert!(
            shape_refusal("codex", governed)
                .unwrap()
                .contains("classifier-governed")
        );
        let restricted = RunShape {
            tool_policy: ToolPolicy::None,
            ..shape()
        };
        assert!(
            shape_refusal("codex", restricted)
                .unwrap()
                .contains("cannot honour ToolPolicy::None")
        );
    }

    #[test]
    fn an_engine_without_an_adapter_has_no_verdict() {
        assert_eq!(shape_refusal("agy", shape()), None);
        assert_eq!(shape_refusal("zed", shape()), None);
    }

    #[test]
    fn the_model_of_an_engine_is_the_one_its_field_holds() {
        let models = crate::config::ModelsConfig::default();
        let default_model = super::super::default_model;
        assert_eq!(
            model_of(&models, "claude"),
            Some(models.claude_model.as_str())
        );
        assert_eq!(
            model_of(&models, "codex"),
            Some(models.codex_model.as_str())
        );
        assert_eq!(model_of(&models, "agy"), None);
        assert_eq!(
            default_model(&models, RunnerKind::Claude),
            models.claude_model
        );
        assert_eq!(
            default_model(&models, RunnerKind::Codex),
            models.codex_model
        );
    }

    #[test]
    fn a_picker_row_without_an_engine_belongs_to_claude() {
        let mut choice = crate::config::ModelsConfig::default()
            .assistant_choices
            .first()
            .cloned()
            .expect("the default file has a picker row");
        choice.runner = None;
        assert_eq!(choice_runner(&choice), "claude");
        choice.runner = Some("codex".to_owned());
        assert_eq!(choice_runner(&choice), "codex");
    }

    #[test]
    fn the_registered_kinds_are_claude_then_codex() {
        assert_eq!(
            registered_kinds(),
            vec![RunnerKind::Claude, RunnerKind::Codex]
        );
    }
}
