//! The pure half of the generalisation pass (spec 5): subjects, reach, group memory, order, seal.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

pub const MIN_SUBJECTS: usize = 3;
pub const MAX_PASS_GROUPS_PER_DAY: usize = 3;
pub const MAX_PASS_ATTEMPTS: i64 = 3;
pub const MAX_EVIDENCE: usize = 20;
pub const MEMORY_CEILING: usize = 200;
pub const MAX_GROUP_SIZE: usize = 12;
pub const CLUSTER_THRESHOLD: f32 = 0.80;
pub const PASS_EVERY: chrono::Duration = chrono::Duration::hours(24);

/// A `knowledge` row as the pass reads it.
#[derive(Clone, Debug)]
pub struct Member {
    pub id: i64,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub evidence: Option<String>,
    pub title: String,
    pub body: String,
}

/// What a group is seen across. The variant order is the set order (chats, jobs, weeks, notes).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    Chat(i64),
    Job(i64),
    Week(String),
    Note(i64),
}

impl Subject {
    /// `"job:4"`, `"chat:7"`, `"week:2026-W43"`, `"note:9"`.
    pub fn key(&self) -> String {
        match self {
            Subject::Job(id) => format!("job:{id}"),
            Subject::Chat(id) => format!("chat:{id}"),
            Subject::Week(label) => format!("week:{label}"),
            Subject::Note(id) => format!("note:{id}"),
        }
    }

    /// Only conversations, weeks and notes can prove a lesson is about the owner.
    pub fn proves_owner(&self) -> bool {
        !matches!(self, Subject::Job(_))
    }
}

/// The chat id of `"<project|->:chat:<n>"`: the LAST marker (a project id may contain `:`) and a digits-only suffix.
pub fn chat_of(scope_id: &str) -> Option<i64> {
    let (_, n) = scope_id.rsplit_once(":chat:")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The project of a chat scope; `-` and an empty prefix are no project.
pub fn chat_project(scope_id: &str) -> Option<&str> {
    chat_of(scope_id)?;
    let project = scope_id.rsplit_once(":chat:")?.0;
    (!project.is_empty() && project != "-").then_some(project)
}

impl Member {
    /// `project` rows: their `scope_id`; `observation` rows: `chat_project`; the rest: none.
    pub fn project(&self) -> Option<&str> {
        match (self.scope_kind.as_str(), self.scope_id.as_deref()) {
            ("project", Some(p)) => Some(p),
            ("observation", Some(s)) => chat_project(s),
            _ => None,
        }
    }

    /// observation: `chat_of(scope_id)` -> Chat, a `week:` prefix -> Week; evidence (serde_json array, malformed = none):
    /// `{"t":"job"}` -> Job, `{"t":"chat"}` -> Chat; `run` and the rest are ignored.
    pub fn subjects(&self) -> BTreeSet<Subject> {
        let mut out = BTreeSet::new();
        if self.scope_kind == "observation"
            && let Some(scope) = self.scope_id.as_deref()
        {
            if let Some(chat) = chat_of(scope) {
                out.insert(Subject::Chat(chat));
            } else if let Some(label) = scope.strip_prefix("week:") {
                out.insert(Subject::Week(label.to_owned()));
            }
        }
        let parsed = self
            .evidence
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
            .unwrap_or_default();
        for entry in parsed {
            let Some(id) = entry.get("id").and_then(serde_json::Value::as_i64) else {
                continue;
            };
            match entry.get("t").and_then(serde_json::Value::as_str) {
                Some("job") => {
                    out.insert(Subject::Job(id));
                }
                Some("chat") => {
                    out.insert(Subject::Chat(id));
                }
                _ => {}
            }
        }
        out
    }
}

pub fn subjects_of(members: &[Member]) -> BTreeSet<Subject> {
    members.iter().flat_map(Member::subjects).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    Machine,
    Project(String),
    Nowhere,
}

pub struct Placement {
    pub reach: Reach,
    pub about: Option<&'static str>,
}

/// Where the lesson lives and whether it is about the owner, in code (spec 5.3): `about` needs the model's claim AND two
/// subjects that are conversations, weeks or notes; notes count no project.
pub fn scope_for(
    projects: &BTreeSet<String>,
    subjects: &BTreeSet<Subject>,
    claims_owner: bool,
) -> Placement {
    let about = (claims_owner && subjects.iter().filter(|s| s.proves_owner()).count() >= 2)
        .then_some("owner");
    let reach = if projects.len() >= 2 || about.is_some() {
        Reach::Machine
    } else if let Some(only) = projects.iter().next() {
        Reach::Project(only.clone())
    } else {
        Reach::Nowhere
    };
    Placement { reach, about }
}

pub const MEMORY_PREFIX: &str = "generalise.group.";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GroupMemory {
    pub h: String,
    pub s: Vec<String>,
}

/// sha256 of the sorted ids joined by `,`, first 16 hex digits.
pub fn members_hash(ids: &[i64]) -> String {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    let joined = sorted
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let digest = Sha256::digest(joined.as_bytes());
    let mut hex = format!("{digest:x}");
    hex.truncate(16);
    hex
}

pub fn memory_key(min_id: i64) -> String {
    format!("{MEMORY_PREFIX}{min_id}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Novelty {
    NewSubjects,
    Fresh,
    Known,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub min_id: i64,
    pub hash: String,
    pub subjects: BTreeSet<String>,
    pub memory: Option<GroupMemory>,
}

impl Candidate {
    pub fn novelty(&self) -> Novelty {
        let Some(m) = &self.memory else {
            return Novelty::Fresh;
        };
        if m.h == self.hash || self.subjects.iter().all(|s| m.s.contains(s)) {
            Novelty::Known
        } else {
            Novelty::NewSubjects
        }
    }
}

/// Groups worth a call, best first: new subjects, then no memory, then more subjects, then the smaller id. Groups under
/// `MIN_SUBJECTS` and groups already answered drop out.
pub fn order_groups(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut eligible: Vec<(u8, Candidate)> = candidates
        .into_iter()
        .filter(|c| c.subjects.len() >= MIN_SUBJECTS)
        .filter_map(|c| match c.novelty() {
            Novelty::NewSubjects => Some((0, c)),
            Novelty::Fresh => Some((1, c)),
            Novelty::Known => None,
        })
        .collect();
    eligible.sort_by(|(ka, a), (kb, b)| {
        ka.cmp(kb)
            .then(b.subjects.len().cmp(&a.subjects.len()))
            .then(a.min_id.cmp(&b.min_id))
    });
    eligible.into_iter().map(|(_, c)| c).collect()
}

/// Memory keys to delete: groups that are gone, then the oldest beyond `MEMORY_CEILING`. Keys not under the prefix are ignored.
pub fn stale_keys(keys: &[String], live_min_ids: &BTreeSet<i64>) -> Vec<String> {
    let mut stale = Vec::new();
    let mut live = Vec::new();
    for key in keys {
        let Some(id) = key
            .strip_prefix(MEMORY_PREFIX)
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        if live_min_ids.contains(&id) {
            live.push(id);
        } else {
            stale.push(key.clone());
        }
    }
    live.sort_unstable();
    let excess = live.len().saturating_sub(MEMORY_CEILING);
    stale.extend(live.into_iter().take(excess).map(memory_key));
    stale
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seal {
    pub at: Option<DateTime<Utc>>,
    pub attempts: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed,
}

pub fn due(seal: &Seal, now: DateTime<Utc>) -> bool {
    seal.at.is_none_or(|at| now - at >= PASS_EVERY)
}

/// Sealed BEFORE the model is asked: a persistent cloud failure must not repeat on every 30 s tick.
pub fn seal_start(seal: &Seal, now: DateTime<Utc>) -> Seal {
    Seal {
        at: Some(now),
        attempts: seal.attempts + 1,
    }
}

/// What the attempt leaves behind. A retry becomes "due" by moving the stamp back one pass period.
pub fn settle(started: &Seal, outcome: Outcome, now: DateTime<Utc>) -> Seal {
    let spent = Seal {
        at: Some(now),
        attempts: 0,
    };
    match outcome {
        Outcome::Done => spent,
        Outcome::Failed if started.attempts >= MAX_PASS_ATTEMPTS => spent,
        Outcome::Failed => match crate::distill::retry_at(started.attempts, now) {
            Some(retry) => Seal {
                at: Some(retry - PASS_EVERY),
                attempts: started.attempts,
            },
            None => spent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn member(id: i64, kind: &str, scope: Option<&str>, evidence: Option<&str>) -> Member {
        Member {
            id,
            scope_kind: kind.into(),
            scope_id: scope.map(Into::into),
            evidence: evidence.map(Into::into),
            title: String::new(),
            body: String::new(),
        }
    }
    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn a_chat_scope_reads_by_its_last_chat_marker_and_digits_only() {
        // (scope_id, project, chat id)
        let table = [
            ("alpha:chat:5", Some("alpha"), Some(5)),
            ("a:chat:5:chat:9", Some("a:chat:5"), Some(9)),
            ("-:chat:3", None, Some(3)),
            (":chat:4", None, Some(4)),
            ("week:2026-W43", None, None),
            ("alpha:chat:", None, None),
            ("alpha:chat:x", None, None),
        ];
        for (scope, project, chat) in table {
            assert_eq!(chat_project(scope), project, "{scope}");
            assert_eq!(chat_of(scope), chat, "{scope}");
        }
    }

    #[test]
    fn a_chat_counts_by_chat_id_never_by_run_and_a_job_by_job() {
        let rows = [
            member(
                1,
                "project",
                Some("alpha"),
                Some(r#"[{"t":"chat","id":7},{"t":"run","id":100}]"#),
            ),
            member(
                2,
                "observation",
                Some("alpha:chat:7"),
                Some(r#"[{"t":"chat","id":7},{"t":"run","id":101}]"#),
            ),
            member(
                3,
                "project",
                Some("alpha"),
                Some(r#"[{"t":"job","id":4},{"t":"run","id":102},{"t":"run","id":103}]"#),
            ),
            member(4, "observation", Some("week:2026-W43"), None),
            member(5, "project", Some("alpha"), Some("not json")),
        ];
        let keys: Vec<String> = subjects_of(&rows).iter().map(Subject::key).collect();
        assert_eq!(keys, vec!["chat:7", "job:4", "week:2026-W43"]);
    }

    #[test]
    fn scope_for_table() {
        use Subject::*;
        let s = |v: Vec<Subject>| -> BTreeSet<Subject> { v.into_iter().collect() };
        let jobs = s(vec![Job(1), Job(2), Job(3)]);
        let chats = s(vec![Chat(1), Chat(2), Job(3)]);
        let one_chat = s(vec![Chat(1), Job(2), Job(3)]);
        let notes = s(vec![Note(1), Note(2)]);
        // (projects, subjects, model claims owner) -> (reach, about)
        let table = [
            (set(&["a", "b"]), &jobs, false, Reach::Machine, None),
            (set(&["a"]), &jobs, false, Reach::Project("a".into()), None),
            (set(&["a"]), &chats, true, Reach::Machine, Some("owner")),
            (
                set(&["a"]),
                &one_chat,
                true,
                Reach::Project("a".into()),
                None,
            ), // one chat is no proof
            (set(&["a"]), &jobs, true, Reach::Project("a".into()), None), // jobs prove nothing about her
            (set(&[]), &notes, true, Reach::Machine, Some("owner")), // notes prove, count no project
            (set(&[]), &jobs, false, Reach::Nowhere, None),
        ];
        for (projects, subjects, claims, reach, about) in table {
            let got = scope_for(&projects, subjects, claims);
            assert_eq!(
                (got.reach, got.about),
                (reach, about),
                "{projects:?} {subjects:?} {claims}"
            );
        }
    }

    fn cand(
        min_id: i64,
        ids: &[i64],
        subjects: &[&str],
        memory: Option<(&[i64], &[&str])>,
    ) -> Candidate {
        Candidate {
            min_id,
            hash: members_hash(ids),
            subjects: set(subjects),
            memory: memory.map(|(m, s)| GroupMemory {
                h: members_hash(m),
                s: s.iter().map(|x| (*x).into()).collect(),
            }),
        }
    }

    #[test]
    fn the_hash_ignores_member_order_and_the_key_names_the_smallest_id() {
        assert_eq!(members_hash(&[3, 1, 2]), members_hash(&[1, 2, 3]));
        assert_ne!(members_hash(&[1, 2, 3]), members_hash(&[1, 2, 4]));
        assert_eq!(memory_key(42), "generalise.group.42");
    }

    #[test]
    fn a_group_is_asked_again_only_with_a_new_subject() {
        let s = ["job:1", "job:2", "job:3"];
        assert_eq!(
            cand(1, &[1, 2, 3], &s, Some((&[1, 2, 3], &s))).novelty(),
            Novelty::Known
        );
        // a new member of an old job is no new subject
        assert_eq!(
            cand(1, &[1, 2, 3, 4], &s, Some((&[1, 2, 3], &s))).novelty(),
            Novelty::Known
        );
        let more = ["job:1", "job:2", "job:3", "chat:9"];
        assert_eq!(
            cand(1, &[1, 2, 3, 5], &more, Some((&[1, 2, 3], &s))).novelty(),
            Novelty::NewSubjects
        );
        assert_eq!(cand(1, &[1, 2, 3], &s, None).novelty(), Novelty::Fresh);
    }

    #[test]
    fn eligible_groups_come_new_subjects_first_then_fresh_then_wider_then_older() {
        let s3 = ["job:1", "job:2", "job:3"];
        let order: Vec<i64> = order_groups(vec![
            cand(30, &[30, 31, 32], &["job:a", "chat:4", "week:W2"], None),
            cand(2, &[2, 3, 4], &s3, Some((&[2, 3, 4], &s3))), // known: out
            cand(40, &[40, 41], &["job:z", "chat:5"], None),   // under 3 subjects: out
            cand(
                20,
                &[20, 21, 22, 23],
                &["job:5", "job:6", "job:7", "job:8"],
                None,
            ), // fresh, wider
            cand(
                1,
                &[1, 2, 3, 9],
                &["job:1", "job:2", "job:3", "chat:1"],
                Some((&[1, 2, 3], &s3)),
            ), // news
            cand(10, &[10, 11, 12], &["job:9", "chat:3", "week:W1"], None), // fresh, older
        ])
        .iter()
        .map(|c| c.min_id)
        .collect();
        assert_eq!(order, vec![1, 20, 10, 30]);
    }

    #[test]
    fn memory_of_vanished_groups_is_pruned_and_the_ceiling_drops_the_oldest() {
        let keys: Vec<String> = [
            "generalise.group.1",
            "generalise.group.5",
            "generalise.group.9",
            "unrelated",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            stale_keys(&keys, &BTreeSet::from([5, 9])),
            vec!["generalise.group.1"]
        );
        let many: Vec<String> = (1..=205).map(memory_key).collect();
        let stale = stale_keys(&many, &(1..=205).collect());
        assert_eq!(stale.len(), 5);
        assert!(stale.contains(&"generalise.group.1".to_owned()));
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_pass_is_due_when_never_sealed_or_sealed_a_day_ago() {
        assert!(due(
            &Seal {
                at: None,
                attempts: 0
            },
            t0()
        ));
        let sealed = |ago| Seal {
            at: Some(t0() - ago),
            attempts: 0,
        };
        assert!(!due(&sealed(Duration::hours(23)), t0()));
        assert!(due(&sealed(Duration::hours(24)), t0()));
    }

    #[test]
    fn the_seal_is_written_at_the_start_and_success_clears_the_attempts() {
        let started = seal_start(
            &Seal {
                at: None,
                attempts: 1,
            },
            t0(),
        );
        assert_eq!((started.at, started.attempts), (Some(t0()), 2));
        let done = settle(&started, Outcome::Done, t0());
        assert_eq!((done.at, done.attempts), (Some(t0()), 0));
    }

    #[test]
    fn a_failed_attempt_retries_on_the_backoff_until_the_third_then_waits_a_day() {
        let fresh = Seal {
            at: None,
            attempts: 0,
        };
        let s1 = settle(&seal_start(&fresh, t0()), Outcome::Failed, t0());
        assert_eq!(s1.attempts, 1);
        assert!(!due(&s1, t0() + Duration::minutes(9)) && due(&s1, t0() + Duration::minutes(10)));
        let s2 = settle(&seal_start(&s1, t0()), Outcome::Failed, t0());
        assert_eq!(s2.attempts, 2);
        assert!(!due(&s2, t0() + Duration::minutes(19)) && due(&s2, t0() + Duration::minutes(20)));
        let s3 = settle(&seal_start(&s2, t0()), Outcome::Failed, t0());
        assert_eq!((s3.at, s3.attempts), (Some(t0()), 0));
        assert!(!due(&s3, t0() + Duration::hours(23)));
    }
}
