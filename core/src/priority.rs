/// What the policy decided, and which rule decided it.
///
/// The reason travels with the class because the class alone cannot be audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub class: &'static str,
    /// The rule that decided the class, or `None` when the model's answer stood untouched.
    ///
    /// This names the rule that DECIDED, not merely one that changed the value: a human pin is
    /// reported even when it agrees with the model, because the pin is why the class is what it
    /// is.
    pub rule: Option<&'static str>,
}

/// A standing human decision that this sender's mail is always urgent.
pub const PIN: &str = "pin";
/// A standing human decision that this sender's mail is always noise.
pub const MUTE: &str = "mute";

/// Whether a stored override is one this policy acts on.
///
/// The `match` below falls through for anything else, so an unrecognised verdict in
/// `contact_overrides` is not an error — it is a row that does nothing, silently, forever. That is
/// the wrong place to discover a typo, so the write endpoint asks this question first and refuses;
/// this function is what keeps the two ends from drifting apart.
pub fn is_known_verdict(verdict: &str) -> bool {
    verdict == PIN || verdict == MUTE
}

pub fn adjust(
    model_class: &str,
    profile: Option<&crate::contacts::Profile>,
    override_verdict: Option<&str>,
) -> Decision {
    match override_verdict {
        Some(PIN) => {
            return Decision {
                class: "urgent",
                rule: Some("human-pin"),
            };
        }
        Some(MUTE) => {
            return Decision {
                class: "noise",
                rule: Some("human-mute"),
            };
        }
        _ => {}
    }

    let Some(model_class) = crate::triage::VALID_CLASSES
        .iter()
        .copied()
        .find(|valid_class| *valid_class == model_class)
    else {
        // Turning an unrecognised model answer into `noise` is the quietest thing this function
        // does; naming it keeps the classifier's loudest failure from becoming its most invisible
        // outcome.
        return Decision {
            class: "noise",
            rule: Some("unknown-class"),
        };
    };
    let unknown_first_contact = match profile {
        None => true,
        Some(profile) => profile.messages_in <= 1 && !profile.outbound_ever,
    };

    if unknown_first_contact && model_class == "urgent" {
        Decision {
            class: "action",
            rule: Some("first-contact"),
        }
    } else {
        Decision {
            class: model_class,
            rule: None,
        }
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
    use super::{Decision, adjust, rank};
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
                    rank(adjusted.class) <= rank(model_class),
                    "derived policy promoted {model_class} to {}",
                    adjusted.class
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
                adjust(model_class, profile, override_verdict).class,
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
                    adjust(model_class, profile, None).class,
                    expected,
                    "{name} with model class {model_class}"
                );
            }
        }
    }

    #[test]
    fn a_regra_que_decidiu_e_nomeada() {
        let first_contact = profile(1, false);
        let has_outbound = profile(1, true);
        let cases = [
            (
                "a pin decides",
                "urgent",
                Some(&first_contact),
                Some("pin"),
                "urgent",
                Some("human-pin"),
            ),
            (
                "a mute decides",
                "urgent",
                Some(&first_contact),
                Some("mute"),
                "noise",
                Some("human-mute"),
            ),
            (
                "a first contact softens urgency",
                "urgent",
                Some(&first_contact),
                None,
                "action",
                Some("first-contact"),
            ),
            (
                "a known contact keeps it",
                "urgent",
                Some(&has_outbound),
                None,
                "urgent",
                None,
            ),
            (
                "nothing to decide",
                "info",
                Some(&first_contact),
                None,
                "info",
                None,
            ),
        ];

        for (name, model_class, profile, override_verdict, expected_class, expected_rule) in cases {
            assert_eq!(
                adjust(model_class, profile, override_verdict),
                Decision {
                    class: expected_class,
                    rule: expected_rule,
                },
                "{name}"
            );
        }
    }

    #[test]
    fn uma_classe_desconhecida_deixa_de_ser_silenciosa() {
        let established = profile(2, false);

        // This has its own test because turning an unrecognised answer into `noise` is the quietest
        // thing this function does and the one most worth being able to see afterwards.
        assert_eq!(
            adjust("panic", Some(&established), None),
            Decision {
                class: "noise",
                rule: Some("unknown-class"),
            }
        );
    }

    #[test]
    fn uma_sobreposicao_humana_decide_antes_de_tudo() {
        let first_contact = profile(1, false);
        let cases = [
            (
                "pin",
                Decision {
                    class: "urgent",
                    rule: Some("human-pin"),
                },
            ),
            (
                "mute",
                Decision {
                    class: "noise",
                    rule: Some("human-mute"),
                },
            ),
        ];

        for (override_verdict, expected) in cases {
            assert_eq!(
                adjust("panic", Some(&first_contact), Some(override_verdict)),
                expected
            );
        }
    }
}
