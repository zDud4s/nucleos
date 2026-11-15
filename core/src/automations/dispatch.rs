//! The engine: an event comes in, the project's enabled rules that answer it fire once each.
//!
//! `dispatch` touches no git. It writes `automation_firings` and, for a `push`, queues a daemon-only
//! `push-sha` through the git queue; the executor reads the rule again and settles the firing.

use sqlx::SqlitePool;

use super::firings::{self, Firing, NewFiring};
use super::{Action, AutomationRule, CiConclusion, PushedBy, Trigger};

/// What happened, as the engine hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The post-land gate proved `sha` green on the integration branch (a project without a release branch).
    LocalGreen { sha: String, branch: String },
    /// `sha` was promoted to the release branch `branch`.
    Promoted { sha: String, branch: String },
    /// A GitHub Actions run of `workflow` finished on `sha`.
    CiFinished {
        sha: String,
        branch: String,
        workflow: String,
        conclusion: CiConclusion,
        run_id: u64,
    },
}

impl Event {
    fn sha(&self) -> &str {
        match self {
            Self::LocalGreen { sha, .. }
            | Self::Promoted { sha, .. }
            | Self::CiFinished { sha, .. } => sha,
        }
    }

    fn branch(&self) -> &str {
        match self {
            Self::LocalGreen { branch, .. }
            | Self::Promoted { branch, .. }
            | Self::CiFinished { branch, .. } => branch,
        }
    }

    /// The idempotency key: one firing per rule per key. A CI key carries the sha because a rerun
    /// keeps its `run_id`.
    fn key(&self) -> String {
        match self {
            Self::LocalGreen { sha, .. } | Self::Promoted { sha, .. } => format!("green:{sha}"),
            Self::CiFinished { sha, run_id, .. } => format!("{sha}:{run_id}"),
        }
    }
}

/// Whether `rule` exists in the project's automations and is switched on. Fails closed: an
/// unreadable document is `false`.
pub fn automation_enabled(project_id: &str, rule: &str) -> bool {
    crate::project::rules::automations(project_id)
        .ok()
        .is_some_and(|document| document.rule(rule).is_some_and(|found| found.enabled))
}

/// Fires every enabled rule of `project_id` that answers `event`, and returns the firings this call
/// created (empty when nothing matched or everything was a repeat). An unreadable `automations` or
/// `autopilot` document fires nothing.
pub async fn dispatch(pool: &SqlitePool, project_id: &str, event: Event) -> Vec<Firing> {
    let automations = match crate::project::rules::automations(project_id) {
        Ok(automations) => automations,
        Err(error) => {
            tracing::warn!(project_id, %error, "the automations could not be read; nothing fires");
            return Vec::new();
        }
    };
    let release_branch = match crate::project::rules::autopilot(project_id) {
        Ok(rules) => rules.release_branch().map(str::to_owned),
        Err(error) => {
            tracing::warn!(project_id, %error, "the autopilot rules could not be read; nothing fires");
            return Vec::new();
        }
    };
    let mut fired = Vec::new();
    for rule in automations.rules.iter().filter(|rule| rule.enabled) {
        if !answers(rule, &event, release_branch.is_some()) {
            continue;
        }
        if !conditions_hold(pool, project_id, rule, &event).await {
            continue;
        }
        if let Some(firing) = fire(pool, project_id, rule, &event).await {
            fired.push(firing);
        }
    }
    fired
}

/// `published_green` answers `promoted` when the project has a release branch and `local_green`
/// when it has none, never both; `ci_finished` answers a run of the same workflow.
fn answers(rule: &AutomationRule, event: &Event, has_release_branch: bool) -> bool {
    match (&rule.on, event) {
        (Trigger::PublishedGreen, Event::Promoted { .. }) => has_release_branch,
        (Trigger::PublishedGreen, Event::LocalGreen { .. }) => !has_release_branch,
        (Trigger::CiFinished { workflow }, Event::CiFinished { workflow: ran, .. }) => {
            workflow == ran
        }
        _ => false,
    }
}

async fn conditions_hold(
    pool: &SqlitePool,
    project_id: &str,
    rule: &AutomationRule,
    event: &Event,
) -> bool {
    let Some(when) = &rule.when else {
        return true;
    };
    if when
        .branch
        .as_deref()
        .is_some_and(|branch| branch != event.branch())
    {
        return false;
    }
    let Event::CiFinished { conclusion, .. } = event else {
        // `conclusion` and `pushed_by` are refused on any other trigger by `validate`.
        return true;
    };
    if when.conclusion.is_some_and(|wanted| wanted != *conclusion) {
        return false;
    }
    if when.pushed_by == Some(PushedBy::Automation) {
        // The sha an automation last pushed to this branch, whichever rule pushed it.
        return match firings::last_push(pool, project_id, event.branch()).await {
            Ok(Some(push)) => push.sha == event.sha(),
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(project_id, %error, "could not read the last automated push");
                false
            }
        };
    }
    true
}

/// Records the firing and carries out its action. `None` when the rule already fired for this
/// event (or the log could not be written).
async fn fire(
    pool: &SqlitePool,
    project_id: &str,
    rule: &AutomationRule,
    event: &Event,
) -> Option<Firing> {
    let action = match &rule.action {
        Action::Push { .. } => "push",
        Action::OpenIncident { .. } => "open_incident",
    };
    let key = event.key();
    let recorded = firings::record(
        pool,
        &NewFiring {
            project_id,
            rule: &rule.name,
            event_key: &key,
            branch: event.branch(),
            sha: event.sha(),
            action,
            outcome: firings::QUEUED,
            detail: "",
        },
    )
    .await;
    let firing = match recorded {
        Ok(Some(firing)) => firing,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(project_id, rule = %rule.name, %error, "could not record a firing");
            return None;
        }
    };
    let settled = match &rule.action {
        Action::Push { remote } => push(pool, project_id, rule, event, remote).await,
        Action::OpenIncident { .. } => Some((
            firings::UNSUPPORTED,
            "open_incident is executed by the CI watcher (plan C)".to_owned(),
        )),
    };
    let Some((outcome, detail)) = settled else {
        return Some(firing);
    };
    let row = firing_row(pool, &firing, outcome, &detail).await;
    Some(row.unwrap_or(firing))
}

/// Settles a non-queued firing by id and announces it.
async fn firing_row(
    pool: &SqlitePool,
    firing: &Firing,
    outcome: &str,
    detail: &str,
) -> Option<Firing> {
    let settled = sqlx::query_as::<_, Firing>(
        "UPDATE automation_firings SET outcome = ?, detail = ? WHERE id = ?
         RETURNING id, project_id, rule, event_key, branch, sha, action, outcome, detail, at",
    )
    .bind(outcome)
    .bind(detail)
    .bind(firing.id)
    .fetch_optional(pool)
    .await;
    match settled {
        Ok(Some(row)) => {
            firings::announce(pool, &row).await;
            Some(row)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(rule = %firing.rule, %error, "could not settle a firing");
            None
        }
    }
}

/// Queues the push, or says why it did not: `None` when the `push-sha` is queued (the executor
/// settles it), otherwise the outcome and detail to settle with.
async fn push(
    pool: &SqlitePool,
    project_id: &str,
    rule: &AutomationRule,
    event: &Event,
    remote: &str,
) -> Option<(&'static str, String)> {
    use crate::execution::vcs::{Branch, CommitSha, Op, Origin, Remote};
    let branch = event.branch();
    let key = crate::task_claims::machine::remote_diverged_key(branch);
    match crate::task_claims::store::live_by_key(pool, project_id, &key, chrono::Utc::now()).await {
        Ok(Some(_)) => return Some((firings::PAUSED, format!("{key} is open"))),
        Ok(None) => {}
        Err(error) => {
            return Some((
                firings::FAILED,
                format!("could not read the {key} incident: {error}"),
            ));
        }
    }
    let op = match (
        Remote::new(remote),
        Branch::new(branch),
        CommitSha::new(event.sha()),
    ) {
        (Ok(remote), Ok(branch), Ok(sha)) => Op::PushSha {
            remote,
            branch,
            sha,
        },
        _ => {
            return Some((
                firings::FAILED,
                "the remote, branch or sha is not something git accepts".to_owned(),
            ));
        }
    };
    let repo = match crate::execution::vcs::resolve_repo(pool, project_id).await {
        Ok(repo) => repo,
        Err(error) => {
            return Some((
                firings::FAILED,
                format!("the project's repository could not be resolved: {error:?}"),
            ));
        }
    };
    let origin = Origin::Automation {
        rule: rule.name.clone(),
    };
    match crate::execution::vcs::submit_declared(pool, &repo, &op, origin).await {
        Ok(_) => None,
        Err(error) => Some((
            firings::FAILED,
            format!("the git queue refused the push: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::rules::testing::{
        clear, publish_automations, publish_rules, publish_unreadable,
    };

    const PROJECT: &str = "auto-dispatch";
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn push_rule(enabled: bool, when: &str) -> String {
        format!(
            "rules:\n  - name: push-when-green\n    enabled: {enabled}\n    on: {{event: published_green}}\n{when}    do: {{action: push, remote: origin}}\n"
        )
    }

    /// A real (empty) git repository on the roster, so `resolve_repo` answers.
    async fn rostered(pool: &SqlitePool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        let out = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .output()
            .expect("git should start");
        assert!(out.status.success(), "git init failed");
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'active', ?)",
        )
        .bind(PROJECT)
        .bind(dir.path().to_string_lossy().into_owned())
        .execute(pool)
        .await
        .unwrap();
        dir
    }

    fn green(sha: &str, branch: &str) -> Event {
        Event::LocalGreen {
            sha: sha.into(),
            branch: branch.into(),
        }
    }

    fn promoted(sha: &str, branch: &str) -> Event {
        Event::Promoted {
            sha: sha.into(),
            branch: branch.into(),
        }
    }

    fn ci(sha: &str, run_id: u64) -> Event {
        Event::CiFinished {
            sha: sha.into(),
            branch: "integ".into(),
            workflow: "gates".into(),
            conclusion: CiConclusion::Failure,
            run_id,
        }
    }

    type Queued = (String, String, String, String, Option<String>);

    /// `(op, sha, branch, origin, automation_rule)` of every queued request, oldest first.
    async fn queued(pool: &SqlitePool) -> Vec<Queued> {
        sqlx::query_as(
            "SELECT op, json_extract(args, '$.sha'), json_extract(args, '$.branch'), origin, automation_rule
             FROM vcs_requests ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn firing_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM automation_firings")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn without_release_branch() {
        clear(PROJECT, "autopilot");
    }

    #[tokio::test]
    async fn a_disabled_rule_never_fires() {
        publish_automations(PROJECT, &push_rule(false, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );
        assert!(queued(&pool).await.is_empty());
        assert_eq!(firing_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn an_enabled_push_rule_queues_one_push_sha_of_the_green_sha() {
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        let fired = dispatch(&pool, PROJECT, green(SHA_A, "integ")).await;

        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].outcome, "queued");
        assert_eq!(fired[0].action, "push");
        assert_eq!(fired[0].event_key, format!("green:{SHA_A}"));
        assert_eq!(
            queued(&pool).await,
            vec![(
                "push-sha".to_owned(),
                SHA_A.to_owned(),
                "integ".to_owned(),
                "daemon".to_owned(),
                Some("push-when-green".to_owned())
            )]
        );
    }

    #[tokio::test]
    async fn the_same_event_twice_fires_once() {
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        assert_eq!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ")).await.len(),
            1
        );
        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );
        assert_eq!(queued(&pool).await.len(), 1);
        assert_eq!(firing_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn a_restart_does_not_repeat_a_firing() {
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_shared_pool().await;
        let _repo = rostered(&pool).await;
        assert_eq!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ")).await.len(),
            1
        );

        // A new handle on the same database file: the daemon after a restart.
        let file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let reopened = sqlx::SqlitePool::connect(&format!("sqlite://{file}"))
            .await
            .unwrap();

        assert!(
            dispatch(&reopened, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );
        assert_eq!(queued(&reopened).await.len(), 1);
        reopened.close().await;
    }

    #[tokio::test]
    async fn without_a_release_branch_published_green_answers_local_green_only() {
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        assert!(
            dispatch(&pool, PROJECT, promoted(SHA_A, "master"))
                .await
                .is_empty()
        );
        assert_eq!(
            dispatch(&pool, PROJECT, green(SHA_B, "integ")).await.len(),
            1
        );
        assert_eq!(queued(&pool).await.len(), 1);
    }

    #[tokio::test]
    async fn with_a_release_branch_published_green_answers_promoted_only() {
        publish_automations(PROJECT, &push_rule(true, ""));
        publish_rules(PROJECT, "release_branch: master\n");
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );
        assert_eq!(
            dispatch(&pool, PROJECT, promoted(SHA_B, "master"))
                .await
                .len(),
            1
        );
        let rows = queued(&pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].1.as_str(), rows[0].2.as_str()), (SHA_B, "master"));
    }

    #[tokio::test]
    async fn when_branch_filters_the_event_branch() {
        publish_automations(PROJECT, &push_rule(true, "    when: {branch: integ}\n"));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "other"))
                .await
                .is_empty()
        );
        assert_eq!(
            dispatch(&pool, PROJECT, green(SHA_B, "integ")).await.len(),
            1
        );
    }

    #[tokio::test]
    async fn unreadable_automations_or_autopilot_documents_fire_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;

        publish_unreadable(PROJECT, "automations", "broken on purpose");
        without_release_branch();
        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );

        publish_automations(PROJECT, &push_rule(true, ""));
        publish_unreadable(PROJECT, "autopilot", "broken on purpose");
        assert!(
            dispatch(&pool, PROJECT, green(SHA_A, "integ"))
                .await
                .is_empty()
        );

        assert!(queued(&pool).await.is_empty());
        assert_eq!(firing_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn an_open_remote_diverged_incident_pauses_the_rule() {
        use crate::task_claims::machine::{close_remote_diverged, open_remote_diverged_ticket};
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;
        let now = chrono::Utc::now();
        open_remote_diverged_ticket(&pool, PROJECT, "integ", serde_json::json!({}), now)
            .await
            .unwrap();

        let fired = dispatch(&pool, PROJECT, green(SHA_A, "integ")).await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].outcome, "paused");
        assert_eq!(fired[0].detail, "remote-diverged:integ is open");
        assert!(queued(&pool).await.is_empty());

        assert!(
            close_remote_diverged(&pool, PROJECT, "integ", "fixed", now)
                .await
                .unwrap()
        );
        let next = dispatch(&pool, PROJECT, green(SHA_B, "integ")).await;
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].outcome, "queued");
        assert_eq!(queued(&pool).await.len(), 1);
    }

    #[tokio::test]
    async fn a_rejected_push_does_not_block_the_next_green() {
        publish_automations(PROJECT, &push_rule(true, ""));
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        let _repo = rostered(&pool).await;
        dispatch(&pool, PROJECT, green(SHA_A, "integ")).await;
        firings::settle(
            &pool,
            PROJECT,
            "push-when-green",
            SHA_A,
            firings::REJECTED,
            "behind",
        )
        .await
        .unwrap()
        .expect("the queued firing settles");

        assert_eq!(
            dispatch(&pool, PROJECT, green(SHA_B, "integ")).await.len(),
            1
        );
        assert_eq!(queued(&pool).await.len(), 2);
    }

    #[tokio::test]
    async fn pushed_by_automation_matches_only_the_last_pushed_sha() {
        publish_automations(
            PROJECT,
            "rules:\n  - name: watch-ci\n    enabled: true\n    on: {event: ci_finished, workflow: gates}\n    when: {pushed_by: automation, conclusion: failure}\n    do: {action: open_incident, auto_fix: true}\n",
        );
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;
        for sha in [SHA_A, SHA_B] {
            firings::record(
                &pool,
                &NewFiring {
                    project_id: PROJECT,
                    rule: "push-when-green",
                    event_key: &format!("green:{sha}"),
                    branch: "integ",
                    sha,
                    action: "push",
                    outcome: firings::PUSHED,
                    detail: "",
                },
            )
            .await
            .unwrap();
        }

        assert!(
            dispatch(&pool, PROJECT, ci(SHA_A, 1)).await.is_empty(),
            "an older push is not the last one"
        );
        let fired = dispatch(&pool, PROJECT, ci(SHA_B, 2)).await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event_key, format!("{SHA_B}:2"));
    }

    #[tokio::test]
    async fn open_incident_is_recorded_unsupported_in_this_plan() {
        publish_automations(
            PROJECT,
            "rules:\n  - name: watch-ci\n    enabled: true\n    on: {event: ci_finished, workflow: gates}\n    do: {action: open_incident, auto_fix: false}\n",
        );
        without_release_branch();
        let pool = crate::testdb::fresh_pool().await;

        let fired = dispatch(&pool, PROJECT, ci(SHA_A, 7)).await;

        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, "open_incident");
        assert_eq!(fired[0].outcome, "unsupported");
        assert_eq!(
            fired[0].detail,
            "open_incident is executed by the CI watcher (plan C)"
        );
        assert!(queued(&pool).await.is_empty());
    }

    #[test]
    fn automation_enabled_fails_closed() {
        publish_automations(PROJECT, &push_rule(true, ""));
        assert!(automation_enabled(PROJECT, "push-when-green"));
        assert!(!automation_enabled(PROJECT, "no-such-rule"));
        publish_automations(PROJECT, &push_rule(false, ""));
        assert!(!automation_enabled(PROJECT, "push-when-green"));
        publish_unreadable(PROJECT, "automations", "broken");
        assert!(!automation_enabled(PROJECT, "push-when-green"));
    }
}
