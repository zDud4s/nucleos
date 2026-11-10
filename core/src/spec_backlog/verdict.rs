//! The pure "finished" rule: a spec is done only with a green verify and an independent reviewer
//! confirming every criterion against the code. In doubt, or with any fact missing, it is not done.

use super::model::{Criterion, CriterionStatus, Spec};

/// Result of the spec's verify command; anything but a known green counts against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyState {
    Green,
    Red,
    Unknown,
}

/// A run that reviewed criteria, with the root of its lineage when the daemon knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerRun {
    pub run_id: String,
    pub lineage_root_id: Option<String>,
}

/// Facts the daemon resolves before asking for a verdict, so the rule itself stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFacts {
    /// Runs of the tree that implemented the spec (empty for an audit without a tree).
    pub tree_run_ids: Vec<String>,
    /// Lineage roots of that tree.
    pub tree_lineage_roots: Vec<String>,
    /// Reviewer runs the daemon could resolve.
    pub reviewers: Vec<ReviewerRun>,
    /// Evidence strings confirmed to exist, matched by exact string.
    pub existing_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotDoneReason {
    VerifyNotGreen,
    NoCriteria,
    Unresolved {
        ordinal: i64,
        status: CriterionStatus,
    },
    Stale {
        ordinal: i64,
    },
    NoReviewer {
        ordinal: i64,
    },
    ReviewerNotIndependent {
        ordinal: i64,
    },
    EvidenceMissing {
        ordinal: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Done,
    NotDone(Vec<NotDoneReason>),
}

/// Whether the reviewer belongs to the implementing tree's own lineage.
fn inside_tree_lineage(reviewer: &ReviewerRun, facts: &ReviewFacts) -> bool {
    let tree_owns = |id: &str| {
        facts.tree_run_ids.iter().any(|run| run == id)
            || facts.tree_lineage_roots.iter().any(|root| root == id)
    };
    tree_owns(&reviewer.run_id) || reviewer.lineage_root_id.as_deref().is_some_and(tree_owns)
}

/// A spec is done only when verify is green, it has criteria, and every one is met against the
/// current spec sha, by an independent resolved reviewer, with existing evidence. Every failed
/// fact is reported; any missing fact means not done.
pub fn done(
    spec: &Spec,
    criteria: &[Criterion],
    verify: VerifyState,
    facts: &ReviewFacts,
) -> Verdict {
    let mut reasons = Vec::new();
    if verify != VerifyState::Green {
        reasons.push(NotDoneReason::VerifyNotGreen);
    }
    // No criteria is not done for every kind, review and note included.
    if criteria.is_empty() {
        reasons.push(NotDoneReason::NoCriteria);
    }
    for criterion in criteria {
        let ordinal = criterion.ordinal;
        match criterion.status {
            CriterionStatus::Met => {}
            CriterionStatus::Stale => {
                reasons.push(NotDoneReason::Stale { ordinal });
                continue;
            }
            status => {
                reasons.push(NotDoneReason::Unresolved { ordinal, status });
                continue;
            }
        }
        if criterion.spec_sha != spec.content_sha {
            reasons.push(NotDoneReason::Stale { ordinal });
            continue;
        }
        let reviewer = criterion
            .reviewer_run_id
            .as_deref()
            .and_then(|id| facts.reviewers.iter().find(|run| run.run_id == id));
        match reviewer {
            None => reasons.push(NotDoneReason::NoReviewer { ordinal }),
            Some(run) if inside_tree_lineage(run, facts) => {
                reasons.push(NotDoneReason::ReviewerNotIndependent { ordinal });
            }
            Some(_) => {}
        }
        let evidence_ok = criterion.evidence.as_deref().is_some_and(|evidence| {
            !evidence.trim().is_empty() && facts.existing_evidence.iter().any(|e| e == evidence)
        });
        if !evidence_ok {
            reasons.push(NotDoneReason::EvidenceMissing { ordinal });
        }
    }
    if reasons.is_empty() {
        Verdict::Done
    } else {
        Verdict::NotDone(reasons)
    }
}

#[cfg(test)]
mod tests {
    use super::{NotDoneReason, ReviewFacts, ReviewerRun, Verdict, VerifyState, done};
    use crate::spec_backlog::model::{
        Criterion, CriterionKind, CriterionStatus, Spec, SpecKind, SpecStatus,
    };

    const SHA: &str = "sha-current";
    const OLD_SHA: &str = "sha-previous";
    const REVIEWER: &str = "reviewer-run";
    const REVIEWER_ROOT: &str = "reviewer-root";
    const TREE_RUN: &str = "tree-run";
    const TREE_ROOT: &str = "tree-root";
    const EVIDENCE: &str = "core/src/thing.rs:42";

    fn spec(kind: SpecKind) -> Spec {
        Spec {
            id: "spec-1".to_owned(),
            project_id: Some("project-1".to_owned()),
            path: "<state_dir>/specs/thing.md".to_owned(),
            title: "Thing".to_owned(),
            content_sha: SHA.to_owned(),
            kind,
            status: SpecStatus::InProgress,
            status_reason: None,
            tree_id: Some("tree-1".to_owned()),
            tree_launched: false,
            last_audited_sha: None,
            created_at: "2026-10-30T10:00:00+00:00".to_owned(),
            updated_at: "2026-10-30T10:00:00+00:00".to_owned(),
        }
    }

    /// A criterion satisfying every rule: each test breaks exactly the fact it is about, so its
    /// failure can only come from that fact.
    fn met(ordinal: i64) -> Criterion {
        Criterion {
            id: ordinal,
            spec_id: "spec-1".to_owned(),
            ordinal,
            text: format!("criterion {ordinal}"),
            kind: CriterionKind::Behavior,
            verify_hint: None,
            status: CriterionStatus::Met,
            evidence: Some(EVIDENCE.to_owned()),
            reviewer_run_id: Some(REVIEWER.to_owned()),
            spec_sha: SHA.to_owned(),
            checked_at: Some("2026-10-30T11:00:00+00:00".to_owned()),
        }
    }

    fn with_status(ordinal: i64, status: CriterionStatus) -> Criterion {
        Criterion {
            status,
            ..met(ordinal)
        }
    }

    fn reviewed_by(ordinal: i64, run_id: &str) -> Criterion {
        Criterion {
            reviewer_run_id: Some(run_id.to_owned()),
            ..met(ordinal)
        }
    }

    fn with_evidence(ordinal: i64, evidence: Option<&str>) -> Criterion {
        Criterion {
            evidence: evidence.map(str::to_owned),
            ..met(ordinal)
        }
    }

    fn run(run_id: &str, lineage_root_id: Option<&str>) -> ReviewerRun {
        ReviewerRun {
            run_id: run_id.to_owned(),
            lineage_root_id: lineage_root_id.map(str::to_owned),
        }
    }

    /// Facts of a tree that implemented the spec, reviewed by a run outside its lineage.
    fn facts() -> ReviewFacts {
        ReviewFacts {
            tree_run_ids: vec![TREE_RUN.to_owned()],
            tree_lineage_roots: vec![TREE_ROOT.to_owned()],
            reviewers: vec![run(REVIEWER, Some(REVIEWER_ROOT))],
            existing_evidence: vec![EVIDENCE.to_owned()],
        }
    }

    /// `NotDone` with exactly these reasons, in any order: the rule fixes which reasons, not their
    /// sequence.
    fn assert_not_done(verdict: Verdict, expected: Vec<NotDoneReason>) {
        match verdict {
            Verdict::Done => panic!("expected NotDone({expected:?}), got Done"),
            Verdict::NotDone(reasons) => {
                assert_eq!(
                    reasons.len(),
                    expected.len(),
                    "expected {expected:?}, got {reasons:?}"
                );
                for reason in &expected {
                    assert!(
                        reasons.contains(reason),
                        "missing {reason:?} in {reasons:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn done_requires_verify_green() {
        let spec = spec(SpecKind::Spec);
        let criteria = [met(1), met(2)];

        assert_eq!(
            done(&spec, &criteria, VerifyState::Green, &facts()),
            Verdict::Done
        );
        for verify in [VerifyState::Red, VerifyState::Unknown] {
            assert_not_done(
                done(&spec, &criteria, verify, &facts()),
                vec![NotDoneReason::VerifyNotGreen],
            );
        }
    }

    #[test]
    fn done_requires_every_criterion_met() {
        let spec = spec(SpecKind::Spec);

        for status in [CriterionStatus::Open, CriterionStatus::Unmet] {
            let criteria = [met(1), with_status(2, status), met(3)];
            assert_not_done(
                done(&spec, &criteria, VerifyState::Green, &facts()),
                vec![NotDoneReason::Unresolved { ordinal: 2, status }],
            );
        }

        let criteria = [
            met(1),
            with_status(2, CriterionStatus::Open),
            met(3),
            with_status(4, CriterionStatus::Unmet),
        ];
        assert_not_done(
            done(&spec, &criteria, VerifyState::Green, &facts()),
            vec![
                NotDoneReason::Unresolved {
                    ordinal: 2,
                    status: CriterionStatus::Open,
                },
                NotDoneReason::Unresolved {
                    ordinal: 4,
                    status: CriterionStatus::Unmet,
                },
            ],
        );
    }

    #[test]
    fn doubt_is_never_done() {
        let criteria = [met(1), with_status(2, CriterionStatus::Doubt)];

        assert_not_done(
            done(
                &spec(SpecKind::Spec),
                &criteria,
                VerifyState::Green,
                &facts(),
            ),
            vec![NotDoneReason::Unresolved {
                ordinal: 2,
                status: CriterionStatus::Doubt,
            }],
        );
    }

    #[test]
    fn stale_sha_is_never_done() {
        let spec = spec(SpecKind::Spec);

        let stale_status = [met(1), with_status(2, CriterionStatus::Stale)];
        assert_not_done(
            done(&spec, &stale_status, VerifyState::Green, &facts()),
            vec![NotDoneReason::Stale { ordinal: 2 }],
        );

        // Still marked `met`, but judged against an older version of the spec.
        let older_sha = [
            met(1),
            Criterion {
                spec_sha: OLD_SHA.to_owned(),
                ..met(2)
            },
        ];
        assert_not_done(
            done(&spec, &older_sha, VerifyState::Green, &facts()),
            vec![NotDoneReason::Stale { ordinal: 2 }],
        );
    }

    #[test]
    fn reviewer_inside_the_trees_lineage_is_rejected() {
        let spec = spec(SpecKind::Spec);
        let cases = [
            // The reviewer is one of the tree's own runs.
            run(TREE_RUN, Some(REVIEWER_ROOT)),
            // The reviewer descends from the tree's lineage.
            run("descendant-run", Some(TREE_ROOT)),
            // The reviewer is the root of the tree's lineage.
            run(TREE_ROOT, None),
        ];

        for reviewer in cases {
            let criteria = [met(1), reviewed_by(2, &reviewer.run_id)];
            let mut reviews = facts();
            reviews.reviewers.push(reviewer);
            assert_not_done(
                done(&spec, &criteria, VerifyState::Green, &reviews),
                vec![NotDoneReason::ReviewerNotIndependent { ordinal: 2 }],
            );
        }
    }

    #[test]
    fn evidence_path_must_exist() {
        let spec = spec(SpecKind::Spec);

        for evidence in [
            None,
            Some(""),
            Some("   "),
            Some("core/src/never_written.rs:1"),
        ] {
            let criteria = [met(1), with_evidence(2, evidence)];
            // A blank string is refused for being blank, even if it were listed as confirmed.
            let mut reviews = facts();
            reviews.existing_evidence.push(String::new());
            reviews.existing_evidence.push("   ".to_owned());
            assert_not_done(
                done(&spec, &criteria, VerifyState::Green, &reviews),
                vec![NotDoneReason::EvidenceMissing { ordinal: 2 }],
            );
        }
    }

    #[test]
    fn no_criteria_is_not_done() {
        for kind in [
            SpecKind::Spec,
            SpecKind::Review,
            SpecKind::Note,
            SpecKind::Unknown,
        ] {
            assert_not_done(
                done(&spec(kind), &[], VerifyState::Green, &facts()),
                vec![NotDoneReason::NoCriteria],
            );
        }
    }

    #[test]
    fn met_without_a_reviewer_is_not_done() {
        let criteria = [
            met(1),
            Criterion {
                reviewer_run_id: None,
                ..met(2)
            },
        ];

        assert_not_done(
            done(
                &spec(SpecKind::Spec),
                &criteria,
                VerifyState::Green,
                &facts(),
            ),
            vec![NotDoneReason::NoReviewer { ordinal: 2 }],
        );
    }

    #[test]
    fn unresolved_reviewer_run_is_not_done() {
        let criteria = [met(1), reviewed_by(2, "run-the-daemon-could-not-resolve")];

        assert_not_done(
            done(
                &spec(SpecKind::Spec),
                &criteria,
                VerifyState::Green,
                &facts(),
            ),
            vec![NotDoneReason::NoReviewer { ordinal: 2 }],
        );
    }

    #[test]
    fn reasons_accumulate() {
        let criteria = [
            met(1),
            with_status(2, CriterionStatus::Stale),
            with_status(3, CriterionStatus::Doubt),
            Criterion {
                reviewer_run_id: None,
                ..met(4)
            },
            reviewed_by(5, TREE_RUN),
            with_evidence(6, None),
            // Two faults on one criterion are both reported.
            Criterion {
                reviewer_run_id: None,
                evidence: None,
                ..met(7)
            },
        ];
        let mut reviews = facts();
        reviews.reviewers.push(run(TREE_RUN, None));

        assert_not_done(
            done(&spec(SpecKind::Spec), &criteria, VerifyState::Red, &reviews),
            vec![
                NotDoneReason::VerifyNotGreen,
                NotDoneReason::Stale { ordinal: 2 },
                NotDoneReason::Unresolved {
                    ordinal: 3,
                    status: CriterionStatus::Doubt,
                },
                NotDoneReason::NoReviewer { ordinal: 4 },
                NotDoneReason::ReviewerNotIndependent { ordinal: 5 },
                NotDoneReason::EvidenceMissing { ordinal: 6 },
                NotDoneReason::NoReviewer { ordinal: 7 },
                NotDoneReason::EvidenceMissing { ordinal: 7 },
            ],
        );
    }

    #[test]
    fn audit_without_a_tree_accepts_an_outside_reviewer() {
        let spec = Spec {
            tree_id: None,
            ..spec(SpecKind::Spec)
        };
        let audit = || ReviewFacts {
            tree_run_ids: Vec::new(),
            tree_lineage_roots: Vec::new(),
            ..facts()
        };

        assert_eq!(
            done(&spec, &[met(1), met(2)], VerifyState::Green, &audit()),
            Verdict::Done
        );

        // The absent tree waives independence from it, nothing else.
        let unresolved = [met(1), reviewed_by(2, "run-the-daemon-could-not-resolve")];
        assert_not_done(
            done(&spec, &unresolved, VerifyState::Green, &audit()),
            vec![NotDoneReason::NoReviewer { ordinal: 2 }],
        );
    }
}
