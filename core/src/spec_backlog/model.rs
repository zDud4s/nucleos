//! Spec backlog model: closed vocabularies, the stable spec id and the registry's row types.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Declares a closed vocabulary stored as text: `as_str` and a strict, case-sensitive `parse`.
macro_rules! vocabulary {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(
                #[serde(rename = $text)]
                $variant,
            )+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

vocabulary! {
    /// What a file under `.ai/specs` turned out to be.
    SpecKind {
        Spec => "spec",
        Review => "review",
        Note => "note",
        Unknown => "unknown",
    }
}

vocabulary! {
    /// Where a spec stands in the backlog.
    SpecStatus {
        Discovered => "discovered",
        Extracting => "extracting",
        Extracted => "extracted",
        Audited => "audited",
        Queued => "queued",
        InProgress => "in_progress",
        Done => "done",
        Blocked => "blocked",
        NeedsOwner => "needs_owner",
        Abandoned => "abandoned",
    }
}

vocabulary! {
    CriterionKind {
        Behavior => "behavior",
        Test => "test",
        Doc => "doc",
        Decision => "decision",
    }
}

vocabulary! {
    CriterionStatus {
        Open => "open",
        Met => "met",
        Unmet => "unmet",
        Doubt => "doubt",
        Stale => "stale",
    }
}

vocabulary! {
    DepSource {
        Extracted => "extracted",
        Owner => "owner",
    }
}

/// The stable id of a spec: a sha256 over the project and the `/`-normalised path, hex, so it is the
/// same in every process. The project is length-prefixed (and `None` marked apart from `Some("")`)
/// so the two parts can never run together into the same bytes.
pub fn spec_id(project_id: Option<&str>, rel_path: &str) -> String {
    let path = rel_path.replace('\\', "/");
    let mut hasher = Sha256::new();
    match project_id {
        None => hasher.update(b"-\n"),
        Some(project) => {
            hasher.update(format!("p{}:", project.len()).as_bytes());
            hasher.update(project.as_bytes());
            hasher.update(b"\n");
        }
    }
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub id: String,
    pub project_id: Option<String>,
    pub path: String,
    pub title: String,
    pub content_sha: String,
    pub kind: SpecKind,
    pub status: SpecStatus,
    pub status_reason: Option<String>,
    pub tree_id: Option<String>,
    /// True once the launcher accepted the tree named by `tree_id`; recording a new id clears it.
    #[serde(default)]
    pub tree_launched: bool,
    pub last_audited_sha: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub id: i64,
    pub spec_id: String,
    pub ordinal: i64,
    pub text: String,
    pub kind: CriterionKind,
    pub verify_hint: Option<String>,
    pub status: CriterionStatus,
    pub evidence: Option<String>,
    pub reviewer_run_id: Option<String>,
    pub spec_sha: String,
    pub checked_at: Option<String>,
}

/// A criterion as extracted, before it has an id, an ordinal or a spec sha.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCriterion {
    pub text: String,
    pub kind: CriterionKind,
    pub verify_hint: Option<String>,
    pub status: CriterionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dep {
    pub spec_id: String,
    pub depends_on: String,
    pub source: DepSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub spec_id: String,
    pub question: String,
    pub answer: Option<String>,
    /// `open` or `answered`.
    pub status: String,
    pub created_at: String,
    pub answered_at: Option<String>,
}

/// One row of the owner's single list: a spec question or a tree-job decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerDecision {
    /// `spec_question` or `tree_decision`.
    pub origin: String,
    pub id: String,
    /// The spec id for a spec question, the tree id (possibly empty) for a tree decision.
    pub subject: String,
    pub question: String,
    pub answer: Option<String>,
    pub status: String,
    pub created_at: String,
    pub answered_at: Option<String>,
}

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
