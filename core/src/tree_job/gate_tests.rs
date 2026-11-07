//! The branch gate submits one ticket per HEAD and reads a late verdict on a later call.

use std::sync::Mutex;
use std::time::Duration;

use super::driver::{GateVerdict, NotMeasured};
use super::gate::{GateIo, Stored, gate_at_head, gate_compiled_then_tested};
use crate::verify::plan::{Progress, Ticket};

const WAIT: Duration = Duration::from_millis(1);

fn ticket(id: i64, done: bool, verdict: Option<&str>) -> Ticket {
    Ticket {
        ticket: id,
        done,
        verdict: verdict.map(str::to_owned),
        project_id: "p".to_owned(),
        worktree: "w".to_owned(),
        kind: "test".to_owned(),
        scope: "scope".to_owned(),
        base: None,
        note: None,
        unclaimed: Vec::new(),
        progress: Progress {
            total: 1,
            finished: 0,
            queued: 1,
            running: Vec::new(),
        },
        units: Vec::new(),
        inherited_task_ids: Vec::new(),
    }
}

/// One scripted queue: the ticket the "queue" holds now, and how often a unit was submitted.
#[derive(Default)]
struct Fake {
    stored: Mutex<Option<Stored>>,
    current: Mutex<Option<Ticket>>,
    submitted: Mutex<Vec<i64>>,
    /// What `inherited` answers for a red: the base's incident, when the base has the same red.
    inherited: Mutex<Option<String>>,
}

impl Fake {
    fn set(&self, ticket: Ticket) {
        *self.current.lock().unwrap() = Some(ticket);
    }
    fn submits(&self) -> usize {
        self.submitted.lock().unwrap().len()
    }
}

impl GateIo for Fake {
    async fn stored(&self) -> Result<Option<Stored>, String> {
        Ok(self.stored.lock().unwrap().clone())
    }
    async fn remember(&self, stored: Option<Stored>) -> Result<(), String> {
        *self.stored.lock().unwrap() = stored;
        Ok(())
    }
    async fn read(&self, _id: i64) -> Result<Option<Ticket>, String> {
        Ok(self.current.lock().unwrap().clone())
    }
    async fn wait(&self, _id: i64, _wait: Duration) -> Result<Option<Ticket>, String> {
        Ok(self.current.lock().unwrap().clone())
    }
    async fn inherited(&self, _ticket: &Ticket) -> Option<String> {
        self.inherited.lock().unwrap().clone()
    }
    async fn submit(&self) -> Result<i64, String> {
        let id = 100 + self.submits() as i64;
        self.submitted.lock().unwrap().push(id);
        self.set(ticket(id, false, None));
        Ok(id)
    }
}

#[tokio::test]
async fn a_head_that_does_not_compile_is_red_before_any_test_is_submitted() {
    let check = Fake::default();
    let test = Fake::default();
    assert!(
        gate_compiled_then_tested(&check, &test, "h1", WAIT)
            .await
            .is_err()
    );
    check.set(ticket(100, true, Some("failed")));
    let verdict = gate_compiled_then_tested(&check, &test, "h1", WAIT).await;
    assert!(
        matches!(verdict, Ok(GateVerdict::Red { .. })),
        "{verdict:?}"
    );
    assert_eq!(
        test.submits(),
        0,
        "no test is spent on a head that does not build"
    );
}

#[tokio::test]
async fn a_head_that_compiles_goes_on_to_its_tests() {
    let check = Fake::default();
    let test = Fake::default();
    assert!(
        gate_compiled_then_tested(&check, &test, "h1", WAIT)
            .await
            .is_err()
    );
    check.set(ticket(100, true, Some("passed")));
    assert!(
        gate_compiled_then_tested(&check, &test, "h1", WAIT)
            .await
            .is_err()
    );
    assert_eq!(test.submits(), 1);
    test.set(ticket(100, true, Some("failed")));
    let verdict = gate_compiled_then_tested(&check, &test, "h1", WAIT).await;
    assert!(
        matches!(verdict, Ok(GateVerdict::Red { .. })),
        "{verdict:?}"
    );
}

#[tokio::test]
async fn a_diff_no_check_selects_still_reaches_its_tests() {
    let check = Fake::default();
    let test = Fake::default();
    assert!(
        gate_compiled_then_tested(&check, &test, "h1", WAIT)
            .await
            .is_err()
    );
    check.set(ticket(100, true, Some("nothing_ran")));
    assert!(
        gate_compiled_then_tested(&check, &test, "h1", WAIT)
            .await
            .is_err()
    );
    assert_eq!(test.submits(), 1);
}

#[tokio::test]
async fn a_gate_that_outlasts_the_wait_is_not_submitted_again() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert_eq!(fake.submits(), 1, "one unit in flight per branch and HEAD");
}

#[tokio::test]
async fn a_late_red_verdict_is_recorded_on_the_next_call() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("failed")));
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Ok(GateVerdict::Red { .. })),
        "{verdict:?}"
    );
    assert_eq!(fake.submits(), 1);
}

#[tokio::test]
async fn a_red_the_base_already_has_is_inherited_not_the_branch_s() {
    let fake = Fake::default();
    *fake.inherited.lock().unwrap() = Some("incident #4".to_owned());
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("failed")));
    assert_eq!(
        gate_at_head(&fake, "h1", WAIT).await,
        Ok(GateVerdict::Inherited {
            note: "incident #4".to_owned()
        })
    );
    assert_eq!(fake.submits(), 1);
}

#[tokio::test]
async fn a_green_is_never_asked_whether_it_is_inherited() {
    let fake = Fake::default();
    *fake.inherited.lock().unwrap() = Some("incident #4".to_owned());
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("passed")));
    assert_eq!(
        gate_at_head(&fake, "h1", WAIT).await,
        Ok(GateVerdict::Green)
    );
}

#[tokio::test]
async fn a_late_green_verdict_is_recorded_on_the_next_call() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("passed")));
    assert_eq!(
        gate_at_head(&fake, "h1", WAIT).await,
        Ok(GateVerdict::Green)
    );
    assert_eq!(fake.submits(), 1);
}

#[tokio::test]
async fn a_unit_that_ended_without_measuring_allows_a_fresh_one() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("errored")));
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert_eq!(fake.submits(), 2);
}

#[tokio::test]
async fn a_ticket_still_running_is_answered_as_running() {
    let fake = Fake::default();
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Err(NotMeasured::Running(_))),
        "a ticket still in flight is no failure: {verdict:?}"
    );
}

#[tokio::test]
async fn a_ticket_that_selected_no_unit_is_a_verdict() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("nothing_ran")));
    assert_eq!(
        gate_at_head(&fake, "h1", WAIT).await,
        Ok(GateVerdict::NothingRan)
    );
    assert_eq!(
        fake.submits(),
        1,
        "a ticket that ran nothing is not resubmitted"
    );
}

#[tokio::test]
async fn a_ticket_that_ended_without_measuring_is_a_failure_reported_once() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("errored")));
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Err(NotMeasured::Failed(_))),
        "the ended ticket is reported, while its fresh unit runs: {verdict:?}"
    );
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Err(NotMeasured::Running(_))),
        "the fresh unit is only waited on: {verdict:?}"
    );
    assert_eq!(fake.submits(), 2);
}

#[tokio::test]
async fn a_remembered_ticket_that_vanished_is_a_failure_and_gets_a_fresh_unit() {
    // The live case: the branch remembers ticket 683, and the daemon answers `no ticket 683`.
    let fake = Fake::default();
    fake.remember(Some(Stored {
        ticket: 683,
        head: "h1".to_owned(),
    }))
    .await
    .unwrap();
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    match &verdict {
        Err(NotMeasured::Failed(reason)) => assert!(reason.contains("683"), "{reason}"),
        other => panic!("a vanished ticket is a failure to count: {other:?}"),
    }
    assert_eq!(fake.submits(), 1, "a fresh unit replaces it");
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Err(NotMeasured::Running(_))),
        "the fresh unit is only waited on: {verdict:?}"
    );
    assert_eq!(fake.submits(), 1);
}

#[tokio::test]
async fn a_ticket_seen_ending_without_measuring_is_forgotten() {
    // Submit, then the wait itself sees the ticket end unmeasured.
    struct EndsAtOnce(Fake);
    impl GateIo for EndsAtOnce {
        async fn stored(&self) -> Result<Option<Stored>, String> {
            self.0.stored().await
        }
        async fn remember(&self, stored: Option<Stored>) -> Result<(), String> {
            self.0.remember(stored).await
        }
        async fn read(&self, id: i64) -> Result<Option<Ticket>, String> {
            self.0.read(id).await
        }
        async fn wait(&self, id: i64, _wait: Duration) -> Result<Option<Ticket>, String> {
            Ok(Some(ticket(id, true, Some("errored"))))
        }
        async fn inherited(&self, ticket: &Ticket) -> Option<String> {
            self.0.inherited(ticket).await
        }
        async fn submit(&self) -> Result<i64, String> {
            self.0.submit().await
        }
    }
    let fake = EndsAtOnce(Fake::default());
    let verdict = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(verdict, Err(NotMeasured::Failed(_))),
        "{verdict:?}"
    );
    assert_eq!(
        *fake.0.stored.lock().unwrap(),
        None,
        "a failure already reported is not reported again by the next call"
    );
}

#[tokio::test]
async fn a_regate_of_the_same_red_head_measures_again() {
    // After a decision answer or a rescue the head is unchanged: the red already delivered must
    // not be read back from its old ticket.
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("failed")));
    assert!(matches!(
        gate_at_head(&fake, "h1", WAIT).await,
        Ok(GateVerdict::Red { .. })
    ));
    let again = gate_at_head(&fake, "h1", WAIT).await;
    assert!(
        matches!(again, Err(NotMeasured::Running(_))),
        "a fresh unit is only waited on: {again:?}"
    );
    assert_eq!(fake.submits(), 2, "a fresh ticket was requested");
}

#[tokio::test]
async fn a_regate_of_a_green_head_still_reads_its_ticket() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    fake.set(ticket(100, true, Some("passed")));
    assert_eq!(gate_at_head(&fake, "h1", WAIT).await, Ok(GateVerdict::Green));
    assert_eq!(gate_at_head(&fake, "h1", WAIT).await, Ok(GateVerdict::Green));
    assert_eq!(fake.submits(), 1);
}

#[tokio::test]
async fn a_new_head_gets_its_own_unit() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert!(gate_at_head(&fake, "h2", WAIT).await.is_err());
    assert_eq!(fake.submits(), 2);
}
