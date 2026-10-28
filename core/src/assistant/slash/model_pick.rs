//! `/model <argument>`: which menu choice a typed argument names.
//!
//! Pure over the choices the caller read from the model menu, so the rule is testable without a
//! daemon: exact id first, then label or unique prefix, without regard to case.

use crate::config::AssistantChoice;

/// How many choices a refusal lists.
pub const LISTED: usize = 8;

/// The id the argument names: `Ok(None)` for `default` (unpin), `Ok(Some(id))` for one choice, and
/// `Err(detail)` for none or several, the detail listing up to [`LISTED`] of the near choices.
pub fn resolve_model(
    choices: &[AssistantChoice],
    argument: &str,
) -> Result<Option<String>, String> {
    let wanted = argument.trim().to_lowercase();
    if wanted == "default" {
        return Ok(None);
    }
    // An exact id beats everything, so `gpt` still reaches a choice whose id another extends.
    if let Some(choice) = choices.iter().find(|c| c.id.to_lowercase() == wanted) {
        return Ok(Some(choice.id.clone()));
    }
    // Then an exact label.
    if let Some(choice) = choices.iter().find(|c| c.label.to_lowercase() == wanted) {
        return Ok(Some(choice.id.clone()));
    }
    // Then a prefix of an id or a label; it must name one choice.
    let near: Vec<&AssistantChoice> = choices
        .iter()
        .filter(|c| {
            c.id.to_lowercase().starts_with(&wanted) || c.label.to_lowercase().starts_with(&wanted)
        })
        .collect();
    if let [only] = near.as_slice() {
        return Ok(Some(only.id.clone()));
    }
    let (shown, why): (Vec<&AssistantChoice>, &str) = if near.is_empty() {
        (choices.iter().collect(), "no model matches")
    } else {
        (near, "more than one model matches")
    };
    let listed: Vec<&str> = shown.iter().take(LISTED).map(|c| c.id.as_str()).collect();
    Err(format!(
        "{why} \"{}\"; try one of: {}",
        argument.trim(),
        listed.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, label: &str) -> AssistantChoice {
        AssistantChoice {
            id: id.to_string(),
            label: label.to_string(),
            brain: "cloud".to_string(),
            efforts: Vec::new(),
            runner: None,
            tools: None,
            installed: None,
        }
    }

    #[test]
    fn slash_b_model_resolves_id_label_prefix() {
        let choices = [
            choice("opus", "Opus"),
            choice("sonnet", "Sonnet"),
            choice("fable", "Fable"),
            choice("gpt-5.6-terra", "GPT Terra"),
            choice("gpt-5.6-luna", "GPT Luna"),
        ];
        let resolve = |argument: &str| resolve_model(&choices, argument);

        // An exact id wins, whatever the case.
        assert_eq!(resolve("opus"), Ok(Some("opus".to_string())));
        assert_eq!(resolve("SONNET"), Ok(Some("sonnet".to_string())));
        // Then a label, then a unique prefix of an id or a label.
        assert_eq!(resolve("gpt terra"), Ok(Some("gpt-5.6-terra".to_string())));
        assert_eq!(resolve("fab"), Ok(Some("fable".to_string())));
        assert_eq!(resolve("GPT Lu"), Ok(Some("gpt-5.6-luna".to_string())));
        // `default` unpins.
        assert_eq!(resolve("default"), Ok(None));
        // Nothing matches, and a prefix two choices share is not a choice.
        assert!(resolve("llama").is_err());
        assert!(resolve("gpt").is_err());
    }

    #[test]
    fn slash_b_model_exact_id_beats_a_longer_prefix_match() {
        let choices = [choice("gpt", "Plain"), choice("gpt-long", "Long")];

        assert_eq!(resolve_model(&choices, "gpt"), Ok(Some("gpt".to_string())));
    }

    #[test]
    fn slash_b_model_ambiguous_lists_eight() {
        let choices: Vec<AssistantChoice> = (1..=12)
            .map(|n| choice(&format!("m{n:02}"), &format!("Alpha {n:02}")))
            .collect();

        let detail = resolve_model(&choices, "alpha").expect_err("twelve choices share the prefix");

        for n in 1..=8 {
            assert!(
                detail.contains(&format!("m{n:02}")),
                "m{n:02} missing: {detail}"
            );
        }
        for n in 9..=12 {
            assert!(
                !detail.contains(&format!("m{n:02}")),
                "m{n:02} listed: {detail}"
            );
        }

        // No match lists the first eight of the menu, so the person sees what is on offer.
        let none = resolve_model(&choices, "zzz").expect_err("nothing matches");
        assert!(none.contains("m01") && none.contains("m08"), "{none}");
        assert!(!none.contains("m09"), "{none}");
    }
}
