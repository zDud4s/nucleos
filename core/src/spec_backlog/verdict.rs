//! The pure "finished" rule: a spec is done only with a green verify and an independent reviewer
//! confirming every criterion against the code. In doubt, or with any fact missing, it is not done.
//! A verdict held back by doubt alone is `Inconclusive`: another tree cannot settle a doubt, so it
//! goes to the owner instead.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotDoneReason {
    VerifyNotGreen,
    /// The spec file's header says it is implemented (`Estado: implementado`). Only ever noted in
    /// an `Inconclusive` verdict, as the reason the doubts are weighed toward done.
    DeclaredImplemented,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Done,
    NotDone(Vec<NotDoneReason>),
    /// Not done, but only because of doubt: every unresolved criterion is a doubt (none a clear
    /// miss) and verify is not green, or the header declares the spec implemented. Another tree
    /// cannot settle a doubt, so the owner decides; no tree is opened for it.
    Inconclusive(Vec<NotDoneReason>),
}

/// Whether a spec file's header (the lines before its first `## ` section) declares it
/// implemented: a line starting `Estado:` (after a list marker or bold markup) that says
/// `implementado` or `implementada` and not `parcial`. `Estado anterior:` is not that line.
pub fn declares_implemented(spec_text: &str) -> bool {
    spec_text
        .lines()
        .take_while(|line| !line.trim_start().starts_with("## "))
        .any(|line| {
            let line = line
                .trim_start()
                .trim_start_matches(['-', '*', '>', ' ', '\t']);
            let Some(value) = line.strip_prefix("Estado:") else {
                return false;
            };
            let value = value.to_lowercase();
            (value.contains("implementado") || value.contains("implementada"))
                && !value.contains("parcial")
        })
}

/// Turns a `NotDone` held back by doubt alone into `Inconclusive`; any other verdict is returned as
/// it is. "Doubt alone" is: at least one criterion in doubt, every other reason `VerifyNotGreen`,
/// and verify not green. With `declared_implemented` the header's word also counts, so doubts with
/// a green verify are inconclusive too, and `DeclaredImplemented` is noted among the reasons.
pub fn weigh_doubts(verdict: Verdict, declared_implemented: bool) -> Verdict {
    let Verdict::NotDone(reasons) = verdict else {
        return verdict;
    };
    let mut doubts = 0;
    let mut verify_not_green = false;
    for reason in &reasons {
        match reason {
            NotDoneReason::VerifyNotGreen => verify_not_green = true,
            NotDoneReason::Unresolved {
                status: CriterionStatus::Doubt,
                ..
            } => doubts += 1,
            _ => return Verdict::NotDone(reasons),
        }
    }
    if doubts == 0 || !(verify_not_green || declared_implemented) {
        return Verdict::NotDone(reasons);
    }
    let mut reasons = reasons;
    if declared_implemented {
        reasons.push(NotDoneReason::DeclaredImplemented);
    }
    Verdict::Inconclusive(reasons)
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
    use super::{
        NotDoneReason, ReviewFacts, ReviewerRun, Verdict, VerifyState, declares_implemented, done,
        weigh_doubts,
    };
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
            Verdict::Inconclusive(reasons) => {
                panic!("expected NotDone({expected:?}), got Inconclusive({reasons:?})")
            }
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

    fn doubt(ordinal: i64) -> NotDoneReason {
        NotDoneReason::Unresolved {
            ordinal,
            status: CriterionStatus::Doubt,
        }
    }

    /// The 2026-11-10 audit of the foundation spec: every criterion in doubt and verify not green.
    /// That is not a miss a tree could fix, so it is inconclusive, never NotDone.
    #[test]
    fn only_doubts_and_verify_not_green_is_inconclusive() {
        let spec = spec(SpecKind::Spec);
        let criteria = [
            with_status(1, CriterionStatus::Doubt),
            with_status(2, CriterionStatus::Doubt),
            with_status(3, CriterionStatus::Doubt),
        ];
        for verify in [VerifyState::Unknown, VerifyState::Red] {
            let verdict = weigh_doubts(done(&spec, &criteria, verify, &facts()), false);
            assert_eq!(
                verdict,
                Verdict::Inconclusive(vec![
                    NotDoneReason::VerifyNotGreen,
                    doubt(1),
                    doubt(2),
                    doubt(3),
                ]),
                "{verify:?}"
            );
        }
        // Met criteria beside the doubts do not change that.
        let mixed = [met(1), with_status(2, CriterionStatus::Doubt)];
        assert_eq!(
            weigh_doubts(done(&spec, &mixed, VerifyState::Unknown, &facts()), false),
            Verdict::Inconclusive(vec![NotDoneReason::VerifyNotGreen, doubt(2)])
        );
    }

    #[test]
    fn a_clear_miss_or_another_reason_keeps_it_not_done() {
        let spec = spec(SpecKind::Spec);
        for status in [CriterionStatus::Unmet, CriterionStatus::Open] {
            let criteria = [
                with_status(1, CriterionStatus::Doubt),
                with_status(2, status),
            ];
            let not_done = done(&spec, &criteria, VerifyState::Unknown, &facts());
            let verdict = weigh_doubts(not_done, true);
            assert!(
                matches!(verdict, Verdict::NotDone(_)),
                "{status:?}: {verdict:?}"
            );
        }
        let missing = [
            with_status(1, CriterionStatus::Doubt),
            with_evidence(2, None),
        ];
        let verdict = weigh_doubts(done(&spec, &missing, VerifyState::Unknown, &facts()), true);
        assert!(matches!(verdict, Verdict::NotDone(_)), "{verdict:?}");
        // Verify alone, with no doubt, is a tree's job.
        let verdict = weigh_doubts(done(&spec, &[met(1)], VerifyState::Red, &facts()), true);
        assert_eq!(
            verdict,
            Verdict::NotDone(vec![NotDoneReason::VerifyNotGreen])
        );
        assert_eq!(
            weigh_doubts(Verdict::Done, true),
            Verdict::Done,
            "Done stays Done"
        );
    }

    #[test]
    fn green_doubts_need_the_header_to_be_inconclusive() {
        let spec = spec(SpecKind::Spec);
        let criteria = [met(1), with_status(2, CriterionStatus::Doubt)];
        let not_done = done(&spec, &criteria, VerifyState::Green, &facts());
        assert_eq!(weigh_doubts(not_done.clone(), false), not_done);
        assert_eq!(
            weigh_doubts(not_done, true),
            Verdict::Inconclusive(vec![doubt(2), NotDoneReason::DeclaredImplemented])
        );
        let unknown = done(&spec, &criteria, VerifyState::Unknown, &facts());
        assert_eq!(
            weigh_doubts(unknown, true),
            Verdict::Inconclusive(vec![
                NotDoneReason::VerifyNotGreen,
                doubt(2),
                NotDoneReason::DeclaredImplemented,
            ])
        );
    }

    #[test]
    fn header_estado_implemented_is_read_from_the_header_only() {
        let yes = [
            "# T\n\n- Data: 2026-07-17\n- Estado: implementado (monorepo em master)\n\n## 1. X\n",
            "# T\nEstado: implementada\n",
            "# T\n**Estado:** Implementado em master\n",
        ];
        for text in yes {
            assert!(declares_implemented(text), "{text:?}");
        }
        let no = [
            "# T\n- Estado: parcialmente implementado\n",
            "# T\n- Estado: proposta\n",
            "# T\n- Estado anterior: implementado\n",
            "# T\n- Data: x\n\n## 1. X\nEstado: implementado\n",
            "# T\nO estado: implementado\n",
            "",
        ];
        for text in no {
            assert!(!declares_implemented(text), "{text:?}");
        }
    }

    #[test]
    fn verdict_round_trips_through_json() {
        let verdict = Verdict::Inconclusive(vec![
            NotDoneReason::VerifyNotGreen,
            doubt(1),
            NotDoneReason::DeclaredImplemented,
        ]);
        let json = serde_json::to_string(&verdict).unwrap();
        assert_eq!(serde_json::from_str::<Verdict>(&json).unwrap(), verdict);
    }
}
