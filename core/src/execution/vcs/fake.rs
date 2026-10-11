//! The test-only fake VCS executor.

use super::*;

/// The test double for `VcsExecutor`. `#[cfg(test)]` because every user of it is a test — building it
/// into the daemon would ship an executor that can report a merge it never performed.
#[cfg(any(test, feature = "testkit"))]
pub(super) struct FakeVcsExecutor {
    pub(super) outcome: Outcome,
    /// How long to take before answering. A real merge takes seconds, and a test about what happens
    /// WHILE one runs — a drain abandoned mid-operation — needs a window to abandon it in. The same
    /// reason `FakeTranscriber` carries one.
    // Qualified rather than imported: the import would be unused in the non-test build.
    pub(super) delay: std::time::Duration,
    /// A rendezvous every execution must reach before any of them may answer.
    ///
    /// The only way to assert concurrency without betting on a scheduler: `n` executions in flight
    /// at once release each other, and `n - 1` or fewer never return at all. A test that instead
    /// measured elapsed time would be asserting that two things overlapped by looking at how long
    /// they took, which is a guess on a loaded machine; this is the property itself.
    pub(super) barrier: Option<tokio::sync::Barrier>,
    /// Every request this was handed, in the order it was handed them.
    ///
    /// Recorded rather than counted, because a fake that ignores its argument answers identically
    /// whether the claim gave it the right row or another repository's — and the order is what makes
    /// the drain's FIFO promise checkable at all. Whole `ClaimedRequest`s rather than a tuple of
    /// fields: two adjacent `String`s destructured positionally can be swapped with every assertion
    /// still passing, which is the hazard `claim_next`'s own 5-tuple carries a warning about.
    pub(super) seen: std::sync::Mutex<Vec<ClaimedRequest>>,
    /// One permit per execution entered, so a test can wait for the operation to *begin*.
    ///
    /// A semaphore rather than a `Notify` because permits accumulate: an execution that starts
    /// before anyone is waiting still counts, so there is no wake-up to lose and no order the two
    /// halves have to arrive in. `entered` is what reads it.
    pub(super) started: tokio::sync::Semaphore,
    /// A latch the test opens when it is ready for the operation to answer. One permit per
    /// execution — each waits for its own release rather than sharing one.
    ///
    /// `delay` is the other way to hold an operation open, and for a test that has work to do
    /// *inside* that window it is a bet: the window is a slice of wall clock, and the bet is that
    /// the work fits. On a loaded machine one SQLite write does not always fit —
    /// `an_outcome_the_row_refuses...` lost that bet 8 runs in 20 while a full build ran beside it,
    /// and failed on an assertion about a log line, which is not what went wrong. `started` fixed
    /// the near edge of that window and this fixes the far one; between them the test names its own
    /// interleaving instead of buying it by the millisecond.
    pub(super) held: Option<tokio::sync::Semaphore>,
}
#[cfg(any(test, feature = "testkit"))]
impl FakeVcsExecutor {
    /// `output_tail` is non-empty and deliberately unlike the sha, for the reason
    /// `failing_with`'s doc comment gives about its own two strings: a fake whose two columns
    /// carried the same text could not tell a test that they had been swapped.
    pub(super) fn succeeding_with(sha: &str) -> Self {
        Self::reporting(Outcome::Succeeded {
            sha: Some(sha.into()),
            output_tail: format!("git printed this while succeeding at {sha}"),
        })
    }

    /// Answers, but not immediately.
    pub(super) fn succeeding_slowly(sha: &str, delay: std::time::Duration) -> Self {
        Self {
            delay,
            ..Self::succeeding_with(sha)
        }
    }

    /// Does not answer until the test says so — and never, if it never does.
    ///
    /// Prefer this to `succeeding_slowly` whenever the test does anything while the operation is
    /// running. The duration in `succeeding_slowly` then has to be long enough for that work on
    /// every machine the suite runs on, which is a number nobody can pick; a latch is that number
    /// being unnecessary.
    pub(super) fn succeeding_until_released(sha: &str) -> Self {
        Self {
            held: Some(tokio::sync::Semaphore::new(0)),
            ..Self::succeeding_with(sha)
        }
    }

    /// Answers only once `n` executions are in flight at the same moment — so a caller that runs
    /// them one after the other never gets an answer at all.
    pub(super) fn rendezvous_of(n: usize, sha: &str) -> Self {
        Self {
            barrier: Some(tokio::sync::Barrier::new(n)),
            ..Self::succeeding_with(sha)
        }
    }

    /// `reason` and `output_tail` are deliberately different strings, so a test using this fake
    /// cannot be blind to those two columns being swapped —
    /// `a_failed_request_records_why_and_what_it_printed` uses distinct ones for the same reason.
    ///
    /// Neither is asserted *through* the drain. What that leaves untested is not whether `finish`
    /// writes the columns, which has direct coverage, but whether `drain_once` forwards the
    /// executor's outcome **whole** rather than rebuilding one of its own — and the drain test's
    /// `result_sha` assertion stands for that, one field deep.
    pub(super) fn failing_with(reason: &str) -> Self {
        Self::reporting(Outcome::Failed {
            reason: reason.into(),
            exit_code: Some(1),
            output_tail: format!("git printed this while failing: {reason}"),
        })
    }

    pub(super) fn escalating_with(reason: &str) -> Self {
        Self::reporting(Outcome::Escalated {
            reason: reason.into(),
            exit_code: Some(1),
            output_tail: format!("CONFLICT (content): {reason}"),
        })
    }

    pub(super) fn reporting(outcome: Outcome) -> Self {
        Self {
            outcome,
            delay: std::time::Duration::ZERO,
            barrier: None,
            seen: std::sync::Mutex::new(Vec::new()),
            started: tokio::sync::Semaphore::new(0),
            held: None,
        }
    }

    /// Lets one held execution answer. Panics on a fake that was not built to be held, because a
    /// test releasing an executor that never waits is asserting an ordering it does not have.
    pub(super) fn release(&self) {
        self.held
            .as_ref()
            .expect("only a `succeeding_until_released` fake has anything to release")
            .add_permits(1);
    }

    /// Resolves once at least `n` executions have been entered.
    ///
    /// What it replaces, and why that mattered: a test that has to act *while* an operation runs
    /// used to `sleep` a fixed 10ms first and assume the drain had got there. That is a real-time
    /// budget for a reap and a claim — two SQLite writes — and on a machine also compiling and
    /// running the other thousand tests it is occasionally missed. The test then acts BEFORE the
    /// operation begins: a different scenario wearing the test's name, failing on an assertion that
    /// says nothing about what actually went wrong. The flake was never about how long the suite
    /// took; it was about jitter, and no larger sleep fixes that.
    ///
    /// Waiting on the event itself has no budget to miss. The permits are handed back on the way
    /// out — dropping a `SemaphorePermit` returns it — so this reads the count without consuming it
    /// and a second caller sees the same answer.
    pub(super) async fn entered(&self, n: usize) {
        drop(
            self.started
                .acquire_many(n as u32)
                .await
                .expect("the fake's semaphore is never closed"),
        );
    }

    /// Derived from `seen` rather than kept beside it: a separate counter can drift from the list it
    /// is supposed to describe, and then nothing says which of the two is right.
    pub(super) fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub(super) fn seen(&self) -> Vec<ClaimedRequest> {
        self.seen.lock().unwrap().clone()
    }
}
#[cfg(any(test, feature = "testkit"))]
#[async_trait::async_trait]
impl VcsExecutor for FakeVcsExecutor {
    async fn execute(&self, request: &ClaimedRequest) -> Outcome {
        // Recorded before the delay, not after: a drain abandoned mid-operation never reaches the
        // line after the await, and a test of that case still needs to see the executor was entered.
        // The guard is a temporary so it is dropped at the end of this statement — held across the
        // await it would make this future non-`Send`, which `async_trait` requires.
        self.seen.lock().unwrap().push(request.clone());
        // Signalled here for the same reason `seen` is written here: what a waiting test needs to
        // know is that the operation BEGAN, and after the delay is too late to be that signal.
        self.started.add_permits(1);
        tokio::time::sleep(self.delay).await;
        // Before the barrier rather than after: the barrier is about executions releasing each
        // other, and a held one has not really started until the test lets it. `forget` because the
        // permit is spent — one release, one execution, so a second is held again.
        if let Some(held) = &self.held {
            held.acquire()
                .await
                .expect("the fake's latch is never closed")
                .forget();
        }
        // Held here rather than before the delay so it is the last thing between being entered and
        // answering: whatever else an execution does, it does not finish until its peers arrive.
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        self.outcome.clone()
    }
}
