//! Spec backlog model: closed vocabularies, the stable spec id and the registry's row types.

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_KINDS: [(SpecKind, &str); 4] = [
        (SpecKind::Spec, "spec"),
        (SpecKind::Review, "review"),
        (SpecKind::Note, "note"),
        (SpecKind::Unknown, "unknown"),
    ];
    const SPEC_STATUSES: [(SpecStatus, &str); 10] = [
        (SpecStatus::Discovered, "discovered"),
        (SpecStatus::Extracting, "extracting"),
        (SpecStatus::Extracted, "extracted"),
        (SpecStatus::Audited, "audited"),
        (SpecStatus::Queued, "queued"),
        (SpecStatus::InProgress, "in_progress"),
        (SpecStatus::Done, "done"),
        (SpecStatus::Blocked, "blocked"),
        (SpecStatus::NeedsOwner, "needs_owner"),
        (SpecStatus::Abandoned, "abandoned"),
    ];
    const CRITERION_KINDS: [(CriterionKind, &str); 4] = [
        (CriterionKind::Behavior, "behavior"),
        (CriterionKind::Test, "test"),
        (CriterionKind::Doc, "doc"),
        (CriterionKind::Decision, "decision"),
    ];
    const CRITERION_STATUSES: [(CriterionStatus, &str); 5] = [
        (CriterionStatus::Open, "open"),
        (CriterionStatus::Met, "met"),
        (CriterionStatus::Unmet, "unmet"),
        (CriterionStatus::Doubt, "doubt"),
        (CriterionStatus::Stale, "stale"),
    ];
    const DEP_SOURCES: [(DepSource, &str); 2] = [
        (DepSource::Extracted, "extracted"),
        (DepSource::Owner, "owner"),
    ];

    #[test]
    fn vocabularies_round_trip() {
        for (variant, text) in SPEC_KINDS {
            assert_eq!(variant.as_str(), text);
            assert_eq!(SpecKind::parse(text), Some(variant));
        }
        for (variant, text) in SPEC_STATUSES {
            assert_eq!(variant.as_str(), text);
            assert_eq!(SpecStatus::parse(text), Some(variant));
        }
        for (variant, text) in CRITERION_KINDS {
            assert_eq!(variant.as_str(), text);
            assert_eq!(CriterionKind::parse(text), Some(variant));
        }
        for (variant, text) in CRITERION_STATUSES {
            assert_eq!(variant.as_str(), text);
            assert_eq!(CriterionStatus::parse(text), Some(variant));
        }
        for (variant, text) in DEP_SOURCES {
            assert_eq!(variant.as_str(), text);
            assert_eq!(DepSource::parse(text), Some(variant));
        }
    }

    #[test]
    fn unknown_vocabulary_is_refused() {
        for text in ["", " ", "SPEC", "Spec", "spec ", "other", "open"] {
            assert_eq!(SpecKind::parse(text), None, "{text:?}");
        }
        for text in [
            "",
            "Discovered",
            "DISCOVERED",
            "InProgress",
            "IN_PROGRESS",
            "in-progress",
            "needs-owner",
            "open",
        ] {
            assert_eq!(SpecStatus::parse(text), None, "{text:?}");
        }
        for text in ["", "Behavior", "BEHAVIOR", "spec", "open"] {
            assert_eq!(CriterionKind::parse(text), None, "{text:?}");
        }
        for text in ["", "Open", "OPEN", "discovered", "behavior"] {
            assert_eq!(CriterionStatus::parse(text), None, "{text:?}");
        }
        for text in ["", "Owner", "EXTRACTED", "guess", "open"] {
            assert_eq!(DepSource::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn spec_id_is_stable_and_separator_blind() {
        for project in [None, Some("proj-1")] {
            let id = spec_id(project, ".ai/specs/alpha.md");
            assert!(!id.is_empty());
            assert_eq!(spec_id(project, ".ai/specs/alpha.md"), id);
            assert_eq!(spec_id(project, ".ai\\specs\\alpha.md"), id);
            assert_eq!(spec_id(project, ".ai/specs\\alpha.md"), id);
        }
    }

    #[test]
    fn spec_id_differs_between_projects_and_paths() {
        let path = ".ai/specs/alpha.md";
        assert_ne!(spec_id(None, path), spec_id(Some("proj-1"), path));
        assert_ne!(spec_id(Some("proj-1"), path), spec_id(Some("proj-2"), path));
        assert_ne!(spec_id(None, path), spec_id(None, ".ai/specs/beta.md"));
        assert_ne!(
            spec_id(Some("proj-1"), path),
            spec_id(Some("proj-1"), ".ai/specs/beta.md")
        );

        // The project and the path must not simply run together before hashing.
        assert_ne!(spec_id(Some("ab"), "c.md"), spec_id(Some("a"), "bc.md"));
        assert_ne!(spec_id(Some("p/q"), "a.md"), spec_id(Some("p"), "q/a.md"));
    }
}
