//! The branch gate submits one ticket per HEAD and reads a late verdict on a later call.

use std::sync::Mutex;
use std::time::Duration;

use super::driver::GateVerdict;
use super::gate::{GateIo, Stored, gate_at_head};
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
    async fn submit(&self) -> Result<i64, String> {
        let id = 100 + self.submits() as i64;
        self.submitted.lock().unwrap().push(id);
        self.set(ticket(id, false, None));
        Ok(id)
    }
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
async fn a_new_head_gets_its_own_unit() {
    let fake = Fake::default();
    assert!(gate_at_head(&fake, "h1", WAIT).await.is_err());
    assert!(gate_at_head(&fake, "h2", WAIT).await.is_err());
    assert_eq!(fake.submits(), 2);
}
