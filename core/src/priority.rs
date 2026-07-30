pub fn adjust(
    model_class: &str,
    profile: Option<&crate::contacts::Profile>,
    override_verdict: Option<&str>,
) -> &'static str {
    match override_verdict {
        Some("pin") => return "urgent",
        Some("mute") => return "noise",
        _ => {}
    }

    let model_class = crate::triage::VALID_CLASSES
        .iter()
        .copied()
        .find(|valid_class| *valid_class == model_class)
        .unwrap_or("noise");
    let unknown_first_contact = match profile {
        None => true,
        Some(profile) => profile.messages_in <= 1 && !profile.outbound_ever,
    };

    if unknown_first_contact && model_class == "urgent" {
        "action"
    } else {
        model_class
    }
}

// Consumed by the `o_derivado_nunca_promove` policy invariant test.
#[allow(dead_code)]
fn rank(class: &str) -> u8 {
    match class {
        "urgent" => 3,
        "action" => 2,
        "info" => 1,
        "noise" => 0,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{adjust, rank};
    use crate::contacts::Profile;

    fn profile(messages_in: i64, outbound_ever: bool) -> Profile {
        Profile {
            display_name: None,
            messages_in,
            first_seen: "2026-07-01T00:00:00Z".to_string(),
            last_seen: "2026-07-29T00:00:00Z".to_string(),
            outbound_ever,
        }
    }

    #[test]
    fn o_derivado_nunca_promove() {
        let first_contact = profile(1, false);
        let established = profile(2, false);
        let has_outbound = profile(1, true);
        let profiles = [
            Some(&first_contact),
            Some(&established),
            Some(&has_outbound),
            None,
        ];

        for &model_class in crate::triage::VALID_CLASSES {
            for &profile in &profiles {
                let adjusted = adjust(model_class, profile, None);
                assert!(
                    rank(adjusted) <= rank(model_class),
                    "derived policy promoted {model_class} to {adjusted}"
                );
            }
        }
    }

    #[test]
    fn a_precedencia_respeita_a_ordem() {
        let first_contact = profile(1, false);
        let cases = [
            (
                "pin overrides derived demotion",
                "urgent",
                Some(&first_contact),
                Some("pin"),
                "urgent",
            ),
            (
                "mute overrides model and derived policy",
                "urgent",
                Some(&first_contact),
                Some("mute"),
                "noise",
            ),
            (
                "derived demotion applies without an override",
                "urgent",
                Some(&first_contact),
                None,
                "action",
            ),
        ];

        for (name, model_class, profile, override_verdict, expected) in cases {
            assert_eq!(
                adjust(model_class, profile, override_verdict),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn a_regra_derivada_e_estreita() {
        let first_contact = profile(1, false);
        let zero_messages = profile(0, false);
        let established = profile(2, false);
        let has_outbound = profile(1, true);

        let profiles = [
            ("first contact", Some(&first_contact), true),
            ("zero recorded messages", Some(&zero_messages), true),
            ("missing profile", None, true),
            ("established contact", Some(&established), false),
            ("contact with outbound history", Some(&has_outbound), false),
        ];

        for &model_class in crate::triage::VALID_CLASSES {
            for &(name, profile, is_unknown_first_contact) in &profiles {
                let expected = if is_unknown_first_contact && model_class == "urgent" {
                    "action"
                } else {
                    model_class
                };
                assert_eq!(
                    adjust(model_class, profile, None),
                    expected,
                    "{name} with model class {model_class}"
                );
            }
        }
    }
}
