//! The live-process pool, barriers, idle policy, and reaping.

use super::*;
// Lives with the runner's other stream readings, since the one-shot loop needs it too.
use crate::execution::runner::track_open_tools;

/// How long a conversation's process waits for a turn that may never come.
///
/// Short on purpose. What it buys is a BURST — the turns somebody takes while they are working on
/// something — and a process idle longer than this is one whose next turn is minutes away, where
/// fourteen seconds of start-up is not what anybody is waiting on. A CLI holds a few hundred
/// megabytes while it waits, and a desktop app is the wrong place to spend that on a conversation
/// nobody came back to.
pub(super) const LIVE_IDLE: std::time::Duration = std::time::Duration::from_secs(90);
/// How long a DEVELOPMENT conversation's process waits: one whose last turn called `Agent`/`Task` or
/// ran longer than [`DEV_TURN`] (spec 4.2 item 3; owner 2026-10-05: derived, never a toggle).
pub(super) const DEV_IDLE: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// A turn longer than this marks its conversation as development work.
pub(super) const DEV_TURN: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// At most this many conversation processes alive at once, kept or mid-turn (owner 2026-10-05).
pub(super) const LIVE_CAP: usize = 5;
/// How long after a visible view's last poll a chat still counts as open: three times the shell's
/// blurred cadence (`BLURRED_CADENCE`, `shell/src/app/pacing.ts`), since a visible but unfocused
/// window polls about every 10 seconds.
pub(super) const OPEN_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a background task may pin a kept process past its idle window, counted from the later
/// of the turn's end and the chat's last poll.
pub(super) const BACKGROUND_PIN_CAP: std::time::Duration =
    std::time::Duration::from_secs(2 * 60 * 60);
/// The cap `keep_live` enforces. Unbounded under test: `LIVE_CHATS` is one static shared by every
/// test running in parallel, and a real cap there would evict other tests' processes. `make_room`
/// is tested directly with `LIVE_CAP`.
#[cfg(not(any(test, feature = "testkit")))]
pub(super) const ENFORCED_CAP: usize = LIVE_CAP;
#[cfg(any(test, feature = "testkit"))]
pub(super) const ENFORCED_CAP: usize = usize::MAX;

/// Every `LiveChat` alive, in `LIVE_CHATS` or in a turn's hands. Held by [`LiveCount`].
pub(super) static LIVE_PROCESSES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Counts one `LiveChat` in [`LIVE_PROCESSES`] for exactly as long as it exists, on every way out.
pub(super) struct LiveCount;
impl LiveCount {
    pub(super) fn start() -> Self {
        LIVE_PROCESSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
impl Drop for LiveCount {
    fn drop(&mut self) {
        LIVE_PROCESSES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One conversation's living CLI, kept between turns instead of started again for each.
///
/// **Measured before it was built**, on the real CLI, two turns down one stdin against a fresh spawn
/// that resumes: `init` at 1.5s against 5.8s, the first shell command running at 6.7s against 26.4s,
/// the whole turn finishing in 12.0s against 26-32s. None of the four `SessionStart` hooks fire on
/// the second turn at all, and the shell the `Bash` tool uses is started once rather than once per
/// turn — which is the larger half, and the one nothing in the daemon could otherwise reach.
///
/// **None of it is money.** Turn two's cost came out inside the noise of a resumed spawn's, and the
/// `cache_read` figures were identical to the token: the prompt cache lives at the API, not in the
/// process. What is bought here is wall-clock and nothing else, which is worth saying plainly
/// because the opposite was assumed twice before it was measured.
///
/// The process keeps ONE session across every turn it serves — measured — so a conversation whose
/// process is gone can still be resumed by that id. Losing the process costs speed, never continuity.
pub(super) struct LiveChat {
    /// Where a later turn is written.
    ///
    /// Dropping this closes the CLI's stdin, which ends the process *after* it finishes whatever
    /// turn it is on — measured; it is not a way to interrupt one. That is what `abort` is for.
    pub(super) messages: tokio::sync::mpsc::UnboundedSender<crate::execution::runner::LaterTurn>,
    /// Everything the process says, already split into turns by `runner::TurnSplitter`.
    pub(super) events: tokio::sync::mpsc::UnboundedReceiver<crate::execution::runner::TurnEvent>,
    /// The session every turn of this process shares, once its first `init` has said what it is.
    ///
    /// Shared rather than owned because it becomes known DURING the first turn, from a task reading
    /// the runner's session channel, while this struct is already in the first turn's hands.
    pub(super) session_id: std::sync::Arc<Mutex<Option<String>>>,
    /// How the process is stopped for real, for the turn that was cancelled and cannot wait for a
    /// closed stdin to be honoured.
    pub(super) abort: tokio::task::AbortHandle,
    /// Why the process stopped, once it has, or `None` while it is still standing.
    ///
    /// The one thing a turn served this way could not say. A turn that ANSWERED has no failure to
    /// explain, but one whose process died mid-answer has exactly one useful fact and it is in the
    /// process's stderr — and the case that actually happens is the tool-policy barrier, whose
    /// message names the offending tools. Without this the conversation showed "the process ended"
    /// and the reason went to a log nobody reading the chat can see.
    ///
    /// A `watch` and not a shared cell: the supervisor sets it at the same moment the stream closes,
    /// so a turn noticing the close has to be able to WAIT briefly for it rather than read whatever
    /// happens to be there.
    pub(super) stopped_because: tokio::sync::watch::Receiver<Option<String>>,
    /// Which rung the process was started on, fixed when it was spawned.
    ///
    /// `--permission-mode` is an argument, so a process started to act cannot be asked to stop
    /// acting — and one started to plan cannot be let loose. Kept so it can be COMPARED, exactly as
    /// `cwd` is: a conversation that changed its mind gets a new process rather than a wrong one.
    ///
    /// The whole rung and not a boolean, since `0129`: a conversation can move between five modes,
    /// and a process spawned on one of them is the wrong process for any of the other four whose
    /// command line differs.
    pub(super) permission: crate::execution::runner::Permission,
    /// Where the process is standing, fixed when it was spawned.
    ///
    /// Kept so it can be COMPARED. A conversation's working directory is resolved per turn, so it can differ from the one this process was started
    /// in, and a turn spoken down it would then run in the wrong directory with nothing anywhere
    /// saying so.
    pub(super) cwd: Option<std::path::PathBuf>,
    /// When it last finished a turn, which is what the reaper measures.
    pub(super) idle_since: std::time::Instant,
    /// How long it may stay idle before the reaper takes it; set from its last turn by `keep_live`.
    pub(super) idle_for: std::time::Duration,
    /// Its place in `LIVE_PROCESSES`.
    pub(super) _counted: LiveCount,
    /// Events read between turns and not yet handed to a turn, oldest first. `gather` drains these
    /// before the channel, so nothing read early is lost or reordered.
    pub(super) carried: std::collections::VecDeque<crate::execution::runner::TurnEvent>,
    /// Background tasks seen starting in this process and not yet seen ending. Non-empty pins the
    /// process against the reaper (spec 4.2 item 2, before `chat_tasks` exists).
    pub(super) background: HashSet<String>,
    /// The between-turns reader that owns this process, by token; 0 for none.
    pub(super) watcher: u64,
    /// The fingerprint of the chat's settings this process was spawned with (K1), compared before a
    /// later turn is written to it. The per-turn briefing never enters it.
    pub(super) spawned_with: String,
    /// The session this process was spawned to resume, if any (K2).
    pub(super) resumed: Option<String>,
    /// Tool calls the current turn has opened and not yet seen answered. While any is open the
    /// silence deadline does not apply: a long command is not a hung process.
    pub(super) open_tools: HashSet<String>,
    /// An answer the CLI began on its own that a person's message arrived in the middle of: set
    /// aside whole so it is not handed to that person, and moved to `SET_ASIDE` by `serve_turn` as
    /// soon as the turn is back, to be recorded from there.
    pub(super) preempted: Option<(String, crate::execution::runner::TurnOutcome)>,
}

impl LiveChat {
    /// The one way a `LiveChat` is built.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        messages: tokio::sync::mpsc::UnboundedSender<crate::execution::runner::LaterTurn>,
        events: tokio::sync::mpsc::UnboundedReceiver<crate::execution::runner::TurnEvent>,
        session_id: std::sync::Arc<Mutex<Option<String>>>,
        abort: tokio::task::AbortHandle,
        stopped_because: tokio::sync::watch::Receiver<Option<String>>,
        permission: crate::execution::runner::Permission,
        cwd: Option<std::path::PathBuf>,
        spawned_with: String,
        resumed: Option<String>,
    ) -> Self {
        Self {
            messages,
            events,
            session_id,
            abort,
            stopped_because,
            permission,
            cwd,
            idle_since: std::time::Instant::now(),
            idle_for: LIVE_IDLE,
            _counted: LiveCount::start(),
            carried: std::collections::VecDeque::new(),
            background: HashSet::new(),
            watcher: 0,
            spawned_with,
            resumed,
            open_tools: HashSet::new(),
            preempted: None,
        }
    }
}

impl Drop for LiveChat {
    /// A dropped `LiveChat` is a conversation that moved on, was stopped, or was reaped — and in
    /// every one of those a process still working is working for nobody. Closing stdin would let it
    /// finish the turn it is on first, so the abort is the honest instrument: it drops the runner's
    /// future, whose `TreeKiller` takes the process and everything it spawned down with it.
    fn drop(&mut self) {
        self.abort.abort();
        if let Ok(mut ended) = ENDED_TASKS.lock() {
            ended.remove(&self.process_key());
        }
        let ended = relock(&PROCESS_BARRIERS).remove(&self.process_key());
        if let Some(ended) = ended {
            stop_tasks_of(ended);
        }
    }
}

/// Closes what a dropped process's turns launched. Spawned because `Drop` cannot await; with no
/// runtime (daemon shutting down) startup's `chat_tasks::orphan_running` closes them instead.
pub(super) fn stop_tasks_of(ended: ProcessBarrier) {
    let ProcessBarrier {
        chat_id,
        served,
        pool,
        ..
    } = ended;
    let (Some(pool), Ok(runtime)) = (pool, tokio::runtime::Handle::try_current()) else {
        return;
    };
    runtime.spawn(async move {
        if let Err(error) = crate::chats::tasks::stop_launched_by(&pool, &chat_id, &served).await {
            tracing::warn!(chat_id = %chat_id, %error, "could not close the tasks of a conversation's ended process");
        }
    });
}

/// Whether a kept process of `chat_id`, alive now, served run `run_id`. This, not the
/// `chat_tasks` row, is what lets a task act after its turn: a row can be written `running` after
/// its process is gone.
pub(crate) fn live_process_served(chat_id: &str, run_id: i64) -> bool {
    relock(&PROCESS_BARRIERS)
        .values()
        .any(|p| p.chat_id == chat_id && p.served.contains(&run_id))
}

/// Test-only stand-in for a kept process that served `run_id`; dropping it ends the vouching.
#[cfg(test)]
pub(crate) struct HeldProcess(usize);

#[cfg(test)]
impl Drop for HeldProcess {
    fn drop(&mut self) {
        relock(&PROCESS_BARRIERS).remove(&self.0);
    }
}

/// Keys are odd, so they never equal a real (aligned) `process_key`.
#[cfg(test)]
pub(crate) fn hold_process_serving(chat_id: &str, run_id: i64) -> HeldProcess {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let key = NEXT.fetch_add(2, std::sync::atomic::Ordering::SeqCst);
    relock(&PROCESS_BARRIERS).insert(
        key,
        ProcessBarrier {
            mode: "auto",
            served: vec![run_id],
            chat_id: chat_id.to_owned(),
            pool: None,
        },
    );
    HeldProcess(key)
}

/// What the turns a kept process has served say about the barrier it runs under.
///
/// The mode is the one STAMPED on the latest turn this process served; `served` is every run id it
/// served, so `read_untrusted` can be the strictest of them. A spontaneous turn takes its barrier
/// from here and not from the chat's newest row: that row may be a turn that never reached this
/// process (stopped before it stamped its mode), and copying it would hand the hook a NULL it reads
/// as `auto`.
pub(super) struct ProcessBarrier {
    pub(super) mode: &'static str,
    pub(super) served: Vec<i64>,
    /// The conversation this process belongs to.
    pub(super) chat_id: String,
    /// To close its tasks when it ends. `None` only for the test holder.
    pub(super) pool: Option<SqlitePool>,
}

/// Barriers of the kept processes, by `LiveChat::process_key`. Cleared by `LiveChat`'s `Drop`.
pub(super) static PROCESS_BARRIERS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<usize, ProcessBarrier>>,
> = std::sync::LazyLock::new(Default::default);

/// Records that the process behind `key` served run `run_id` under `mode`.
pub(super) fn note_served(
    key: usize,
    chat_id: &str,
    run_id: i64,
    mode: &'static str,
    pool: &SqlitePool,
) {
    let mut barriers = relock(&PROCESS_BARRIERS);
    let entry = barriers.entry(key).or_insert_with(|| ProcessBarrier {
        mode,
        served: Vec::new(),
        chat_id: chat_id.to_owned(),
        pool: Some(pool.clone()),
    });
    entry.mode = mode;
    entry.served.push(run_id);
}

/// `note_served` for the process a conversation has kept, if it kept one.
pub(super) fn note_served_by_kept(
    chat_id: &str,
    run_id: i64,
    mode: &'static str,
    pool: &SqlitePool,
) {
    let kept = relock(&LIVE_CHATS);
    if let Some(live) = kept.get(chat_id) {
        note_served(live.process_key(), chat_id, run_id, mode, pool);
    }
}

/// Background tasks each kept process has seen END, by `LiveChat::process_key`.
///
/// Kept beside the struct rather than in it. A task id is unique within a process and never starts
/// again once it ended, so a start line naming one that already ended is a replay and not new work:
/// without this, such a line would pin the process against the reaper for good.
pub(super) static ENDED_TASKS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<usize, HashSet<String>>>,
> = std::sync::LazyLock::new(Default::default);

/// What happened when a conversation's living process was spoken to.
///
/// The three are told apart because two of them are safe to start over from and one is not.
pub(super) enum LiveTurn {
    /// It answered.
    Answered(crate::execution::runner::TurnOutcome),
    /// Nothing was written: the process was already gone. The turn can be started fresh, because
    /// as far as anything outside is concerned it never happened.
    NotWritten,
    /// It was written, and the process died before answering, with whatever the process said on
    /// its way out. The turn is lost and must NOT be quietly started again — whatever it had already
    /// done, a command run or a file written, would be done a second time.
    DiedMidTurn(Option<String>),
    /// No event arrived for that long. The turn is lost like `DiedMidTurn`, and the caller drops
    /// the process.
    WentSilent(std::time::Duration),
}

impl LiveChat {
    /// Identifies this process while it lives: the address of its shared session slot, which is
    /// allocated once per process and released with it (`Drop` clears what is keyed by it).
    pub(super) fn process_key(&self) -> usize {
        std::sync::Arc::as_ptr(&self.session_id) as *const () as usize
    }

    /// `track_background` over this process, remembering which tasks ended and never letting an
    /// ended one back in.
    pub(super) fn track(&mut self, line: &str) {
        if !is_task_bookkeeping(line) {
            return;
        }
        let before = self.background.clone();
        track_background(line, &mut self.background);
        let key = self.process_key();
        let Ok(mut ended) = ENDED_TASKS.lock() else {
            return;
        };
        let gone = ended.entry(key).or_default();
        gone.extend(before.difference(&self.background).cloned());
        self.background.retain(|id| !gone.contains(id));
    }

    /// Says something to this process and gathers the one turn it answers with.
    pub(super) async fn turn(
        &mut self,
        text: &str,
        images: &[crate::execution::runner::Attachment],
        transcript: &std::sync::Arc<Mutex<String>>,
        silence: Option<std::time::Duration>,
    ) -> LiveTurn {
        // A process whose stdin is already closed, or that the supervisor has recorded as stopped
        // while its stream is still open, is not written to: the turn never happened, so it can be
        // started fresh. A recorded stop whose stream has ALSO closed falls through to the write and
        // reads as `DiedMidTurn` with its reason, which is what a conversation shows. In production
        // the supervisor records a reason only after the runner has dropped both ends, so the
        // closed stdin is what decides there.
        let recorded_stop = self.stopped_because.borrow().is_some();
        if self.messages.is_closed() || (recorded_stop && !self.events.is_closed()) {
            return LiveTurn::NotWritten;
        }
        // An answer the CLI began on its own before this message belongs to that answer. Set it
        // aside whole, to its own end, so this turn gathers only what answers it.
        if self.drain_idle() && self.answer_carried() {
            let aside = std::sync::Arc::new(Mutex::new(String::new()));
            match self.gather(&aside, silence).await {
                LiveTurn::Answered(outcome) => {
                    let stdout = aside.lock().map(|held| held.clone()).unwrap_or_default();
                    self.preempted = Some((stdout, outcome));
                }
                // The earlier answer never finished: the process is gone or hung, and nothing of
                // this message was written.
                _ => return LiveTurn::NotWritten,
            }
        }
        let said = crate::execution::runner::LaterTurn {
            text: text.to_owned(),
            images: images.to_vec(),
            interrupt: false,
            stop_task: None,
        };
        if self.messages.send(said).is_err() {
            return LiveTurn::NotWritten;
        }
        self.gather(transcript, silence).await
    }

    /// Gathers the turn the process was STARTED with, which travelled in its opening line rather
    /// than down this channel — so there is nothing to send, only an answer to wait for.
    pub(super) async fn opening(
        &mut self,
        transcript: &std::sync::Arc<Mutex<String>>,
        silence: Option<std::time::Duration>,
    ) -> LiveTurn {
        self.gather(transcript, silence).await
    }

    /// Reads one turn's worth of the stream into `transcript`, stopping at its own end.
    ///
    /// Stops at its own `Ended` and not a line later. Reading past it would swallow the opening of
    /// the turn after this one; stopping short would hand that turn the tail of this one. Both fail
    /// the same way from outside — a conversation whose answers are quietly somebody else's — which
    /// is why the boundary is drawn in `runner::TurnSplitter`, where a test can reach it.
    pub(super) async fn gather(
        &mut self,
        transcript: &std::sync::Arc<Mutex<String>>,
        silence: Option<std::time::Duration>,
    ) -> LiveTurn {
        loop {
            // What the between-turns reader already took off the channel comes first, in order.
            let (next, fresh) = match self.carried.pop_front() {
                Some(event) => (Some(event), false),
                None => {
                    let next = match silence {
                        // A tool call still open is work in progress, not silence; the caller's
                        // ceiling still bounds the wait.
                        Some(d) if self.open_tools.is_empty() => {
                            match tokio::time::timeout(d, self.events.recv()).await {
                                Ok(event) => event,
                                Err(_) => return LiveTurn::WentSilent(d),
                            }
                        }
                        _ => self.events.recv().await,
                    };
                    (next, true)
                }
            };
            let Some(event) = next else { break };
            match event {
                crate::execution::runner::TurnEvent::Line(line) => {
                    // Carried lines were tracked when they were read; only a fresh one is new.
                    if fresh {
                        self.track(&line);
                    }
                    // Tool calls are tracked for carried lines too: the between-turns reader does
                    // not look at them.
                    track_open_tools(&line, &mut self.open_tools);
                    // The same accumulation the runner does for a one-turn process, so a turn served
                    // this way leaves the transcript a turn served the other way would have.
                    if let Ok(mut shared) = transcript.lock() {
                        shared.push_str(&line);
                        shared.push('\n');
                    }
                }
                crate::execution::runner::TurnEvent::Ended(outcome) => {
                    self.open_tools.clear();
                    return LiveTurn::Answered(outcome);
                }
            }
        }
        LiveTurn::DiedMidTurn(self.why_it_stopped().await)
    }

    /// Reads whatever the process said since the last turn into `carried`, without waiting.
    ///
    /// Returns whether that includes an answer begun: an assistant or stream-event line, or the end
    /// of an answer. The CLI also echoes user lines, announces itself and reports limits between
    /// turns, and none of that is something a turn of its own should be started on.
    pub(super) fn drain_idle(&mut self) -> bool {
        let was_working = !self.background.is_empty();
        while let Ok(event) = self.events.try_recv() {
            if let crate::execution::runner::TurnEvent::Line(line) = &event {
                self.track(line);
            }
            self.carried.push_back(event);
        }
        if was_working && self.background.is_empty() {
            // The last task just ended: the idle clock starts now, not when the turn before it did.
            self.idle_since = std::time::Instant::now();
        }
        self.carried.iter().any(|event| match event {
            crate::execution::runner::TurnEvent::Line(line) => is_answer_line(line),
            crate::execution::runner::TurnEvent::Ended(_) => true,
        })
    }

    /// Whether the first segment of `carried` — up to and including its first `Ended`, or all of it
    /// when none has arrived — holds an answer line. That segment is what `turn` would set aside;
    /// one with no answer line in it is not an answer the CLI began, and setting it aside would
    /// record a spontaneous turn that says nothing.
    fn answer_carried(&self) -> bool {
        for event in &self.carried {
            match event {
                crate::execution::runner::TurnEvent::Line(line) if is_answer_line(line) => {
                    return true;
                }
                crate::execution::runner::TurnEvent::Line(_) => {}
                crate::execution::runner::TurnEvent::Ended(_) => return false,
            }
        }
        false
    }

    /// What the process said on its way out, if it manages to say it in time.
    ///
    /// Bounded, because this is a diagnosis and not the answer: the stream closing and the
    /// supervisor recording the reason are the same instant from two sides, so waiting is right and
    /// waiting long is not. A turn that has already failed must not also hang.
    pub(super) async fn why_it_stopped(&mut self) -> Option<String> {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.stopped_because.changed(),
        )
        .await;
        self.stopped_because.borrow().clone()
    }
}

/// Takes a conversation's living process out of the registry for the length of one turn.
///
/// Taken OUT rather than borrowed in place, and the ownership is the point. A turn holds the only
/// handle while it runs, so if its task is aborted — `/cancel`, a dropped request, a panic — the
/// handle is dropped with it and `Drop` takes the process down. Nothing has to remember to clean up
/// on a path where nothing gets the chance to run.
pub(super) fn take_live(chat_id: &str) -> Option<LiveChat> {
    relock(&LIVE_CHATS).remove(chat_id)
}

/// Puts a process back, having just finished a turn, for the next one to find.
pub(super) fn keep_live(chat_id: &str, mut live: LiveChat, idle_for: std::time::Duration) {
    let now = std::time::Instant::now();
    live.idle_since = now;
    live.idle_for = idle_for;
    let open: HashSet<String> = relock(&SEEN_CHATS)
        .iter()
        .filter(|(_, at)| is_open(Some(**at), now))
        .map(|(id, _)| id.clone())
        .collect();
    let evicted = {
        let mut kept = relock(&LIVE_CHATS);
        kept.insert(chat_id.to_owned(), live);
        let alive = LIVE_PROCESSES.load(std::sync::atomic::Ordering::SeqCst);
        make_room(&mut kept, alive, chat_id, ENFORCED_CAP, &open)
    };
    // Dropped outside the lock; dropping is what stops them.
    drop(evicted);
    reap_idle_live_chats();
}

/// How long a process may idle after this turn: [`DEV_IDLE`] when the turn called `Agent`/`Task` or
/// lasted longer than [`DEV_TURN`], [`LIVE_IDLE`] otherwise.
pub(super) fn idle_for_turn(stdout: &str, lasted: std::time::Duration) -> std::time::Duration {
    let used_subagents = stdout.lines().any(|line| {
        if !line.contains("\"tool_use\"") {
            return false;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
            && value
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                            && matches!(
                                item.get("name").and_then(serde_json::Value::as_str),
                                Some("Agent" | "Task")
                            )
                    })
                })
    });
    if used_subagents || lasted > DEV_TURN {
        DEV_IDLE
    } else {
        LIVE_IDLE
    }
}

/// How long a process idles after a SPONTANEOUS turn: the later of what it already had and what the
/// turn earns. A background answer is usually a short plain reply, and must not drop a development
/// chat back to [`LIVE_IDLE`] right after its background `Agent` finished. Human turns do not use
/// this: the last turn decides.
pub(super) fn idle_after_spontaneous(
    current: std::time::Duration,
    stdout: &str,
    lasted: std::time::Duration,
) -> std::time::Duration {
    current.max(idle_for_turn(stdout, lasted))
}

/// Takes processes out of `kept` until `alive` fits under `cap`, chats nobody has open first and
/// least recently used within each group, and returns them for the caller to drop. Never one with a
/// background task, nor one whose `carried` still holds an answer no turn has claimed yet (evicting
/// it would lose that answer). The one being kept goes only when nothing else can (spec 4.2 item 3:
/// when every other is busy, the new one is not kept).
pub(super) fn make_room(
    kept: &mut HashMap<String, LiveChat>,
    alive: usize,
    keeping: &str,
    cap: usize,
    open: &HashSet<String>,
) -> Vec<LiveChat> {
    let mut evicted = Vec::new();
    let mut excess = alive.saturating_sub(cap);
    while excess > 0 {
        let oldest = kept
            .iter()
            .filter(|(id, live)| {
                id.as_str() != keeping && live.background.is_empty() && live.carried.is_empty()
            })
            .min_by_key(|(id, live)| (open.contains(id.as_str()), live.idle_since))
            .map(|(id, _)| id.clone());
        let victim = match oldest {
            Some(id) => id,
            None if kept
                .get(keeping)
                .is_some_and(|live| live.background.is_empty() && live.carried.is_empty()) =>
            {
                keeping.to_owned()
            }
            None => break,
        };
        let Some(live) = kept.remove(&victim) else {
            break;
        };
        evicted.push(live);
        excess -= 1;
    }
    evicted
}

/// Whether a turn runs with the user's ambient MCP servers: only when the conversation opted in
/// and the turn is not restricted to NucleOS's own tools.
pub fn ambient_mcp_for(policy: crate::execution::runner::ToolPolicy, opted_in: bool) -> bool {
    opted_in && policy == crate::execution::runner::ToolPolicy::Unrestricted
}

/// Stops a conversation's process for good, if it has one.
pub fn evict_live(chat_id: &str) {
    // The removed value is dropped here, which is what aborts it.
    relock(&LIVE_CHATS).remove(chat_id);
}

/// Starts the one task that stops processes nobody came back to.
///
/// A task rather than a check at the start of the next turn, because the conversation this is about
/// is precisely the one where there is no next turn. Started on the first process kept rather than
/// at boot, so a daemon that never has a rooted conversation never has the task either.
pub(super) fn reap_idle_live_chats() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tick.tick().await;
            reap_now();
        }
    });
}

/// Drops every kept process that is no longer worth keeping.
///
/// Its own function, called by the ticker, so the rule can be asserted without waiting fifteen
/// seconds for a task to decide to run.
pub(super) fn reap_now() {
    let now = std::time::Instant::now();
    // A poll older than the longest idle window can no longer move any anchor, so it is forgotten.
    let seen = {
        let mut seen = relock(&SEEN_CHATS);
        seen.retain(|_, at| now.saturating_duration_since(*at) < DEV_IDLE);
        seen.clone()
    };
    // `retain` drops what it removes, and dropping is what stops the process.
    relock(&LIVE_CHATS).retain(|id, live| worth_keeping(live, seen.get(id).copied(), now));
}

/// Whether a kept process is still worth its memory at `now`, given when its chat was last polled.
pub(super) fn worth_keeping(
    live: &LiveChat,
    seen: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    // A closed stdin is the runner's future having ended — it owns the far end — so the process
    // behind this handle is already gone. Kept entries like that are not merely useless: the
    // next turn survives finding one, because writing to it fails and it starts a process
    // instead, but on a conversation nobody returns to it sits there for good.
    let still_standing = !live.messages.is_closed();
    // A background task that has started and not reported its end is work the process is still
    // doing for the conversation (spec 4.2 item 2; spike 2026-10-05 (c): the CLI answers on its
    // own when it ends), so such a process is kept past its idle window — but only for
    // [`BACKGROUND_PIN_CAP`], or a task that never reports its end pins it for good.
    // The idle time is the chat's own, derived from its last turn, and it counts from the later of
    // that turn's end and the last time the chat was polled by a visible view.
    let anchor = seen.map_or(live.idle_since, |s| s.max(live.idle_since));
    let since = now.saturating_duration_since(anchor);
    still_standing
        && ((!live.background.is_empty() && since < BACKGROUND_PIN_CAP) || since < live.idle_for)
}

/// Whether a stream line is part of an answer: its JSON `type` is `assistant` or `stream_event`.
fn is_answer_line(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(|kind| matches!(kind, "assistant" | "stream_event"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::{LaterTurn, TurnEvent, TurnOutcome};
    use std::time::{Duration, Instant};

    /// A `LiveChat` with its far ends in hand: what the process "says" goes into `events`, and what
    /// is written to it arrives on `said`.
    struct Rig {
        live: LiveChat,
        events: tokio::sync::mpsc::UnboundedSender<TurnEvent>,
        said: tokio::sync::mpsc::UnboundedReceiver<LaterTurn>,
        stopped: tokio::sync::watch::Sender<Option<String>>,
    }

    fn rig() -> Rig {
        let (messages, said) = tokio::sync::mpsc::unbounded_channel();
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (stopped, stopped_rx) = tokio::sync::watch::channel(None);
        let live = LiveChat::new(
            messages,
            events_rx,
            std::sync::Arc::new(Mutex::new(Some("s-1".to_owned()))),
            tokio::spawn(async {}).abort_handle(),
            stopped_rx,
            crate::execution::runner::Permission::Default,
            None,
            String::new(),
            None,
        );
        Rig {
            live,
            events,
            said,
            stopped,
        }
    }

    fn assistant_text(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    fn assistant_tool_use(id: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {}}]}
        })
        .to_string()
    }

    fn user_tool_result(id: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}
        })
        .to_string()
    }

    fn costing(usd: f64) -> TurnOutcome {
        TurnOutcome {
            cost_usd: Some(usd),
            ..Default::default()
        }
    }

    /// Between turns the kept process's stream carries more than answers: the CLI echoes the user
    /// lines it was written, announces itself, reports limits. None of that is an answer, and a turn
    /// of its own started on one is a turn that answers nothing.
    #[tokio::test]
    async fn between_turns_only_an_answer_line_starts_a_turn_of_its_own() {
        let quiet = [
            serde_json::json!({"type": "user", "message": {"content": "hi"}}).to_string(),
            serde_json::json!({"type": "system", "subtype": "init"}).to_string(),
            serde_json::json!({"type": "rate_limit_event"}).to_string(),
            "not json at all".to_owned(),
        ];
        for line in quiet {
            let mut rig = rig();
            rig.events.send(TurnEvent::Line(line.clone())).unwrap();
            assert!(
                !rig.live.drain_idle(),
                "a line that is not an answer began a turn: {line}"
            );
        }

        for line in [
            assistant_text("the build finished"),
            serde_json::json!({"type": "stream_event", "event": {"type": "message_start"}})
                .to_string(),
        ] {
            let mut rig = rig();
            rig.events.send(TurnEvent::Line(line.clone())).unwrap();
            assert!(
                rig.live.drain_idle(),
                "an answer line did not begin a turn: {line}"
            );
        }

        let mut rig = rig();
        rig.events.send(TurnEvent::Ended(costing(0.1))).unwrap();
        assert!(
            rig.live.drain_idle(),
            "the end of an answer must begin a turn"
        );
    }

    /// `track_open_tools` is what `gather` consults: a tool call opens on its `tool_use`, closes on
    /// the matching `tool_result`, and nothing stays open past the end of the turn.
    #[tokio::test]
    async fn a_turn_with_an_open_tool_call_is_not_cut_off_for_silence() {
        let mut open = HashSet::new();
        track_open_tools(&assistant_tool_use("toolu_1"), &mut open);
        assert_eq!(open, HashSet::from(["toolu_1".to_owned()]));
        track_open_tools(&assistant_text("unrelated"), &mut open);
        assert_eq!(open.len(), 1);
        track_open_tools(&user_tool_result("toolu_1"), &mut open);
        assert!(open.is_empty());

        // The same through `gather`: silence is 100 ms, the tool runs for 600 ms, and the turn must
        // still arrive whole.
        let mut rig = rig();
        let events = rig.events.clone();
        tokio::spawn(async move {
            events
                .send(TurnEvent::Line(assistant_tool_use("toolu_2")))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            events
                .send(TurnEvent::Line(user_tool_result("toolu_2")))
                .unwrap();
            events
                .send(TurnEvent::Line(assistant_text("done")))
                .unwrap();
            events.send(TurnEvent::Ended(costing(2.0))).unwrap();
        });
        let transcript = std::sync::Arc::new(Mutex::new(String::new()));
        let served = tokio::time::timeout(
            Duration::from_secs(10),
            rig.live
                .gather(&transcript, Some(Duration::from_millis(100))),
        )
        .await
        .expect("the turn must finish");
        match served {
            LiveTurn::Answered(outcome) => assert_eq!(outcome.cost_usd, Some(2.0)),
            LiveTurn::WentSilent(_) => panic!("a running tool call was cut off for silence"),
            _ => panic!("the turn did not answer"),
        }
        assert!(transcript.lock().unwrap().contains("done"));

        // And a turn with no tool open still goes silent, so the exemption is not a blanket one.
        let mut quiet = self::rig();
        let transcript = std::sync::Arc::new(Mutex::new(String::new()));
        let served = quiet
            .live
            .gather(&transcript, Some(Duration::from_millis(100)))
            .await;
        assert!(matches!(served, LiveTurn::WentSilent(_)));
    }

    /// A spontaneous answer already under way when a person writes is that answer's, not theirs.
    #[tokio::test]
    async fn an_answer_begun_before_a_persons_message_is_not_handed_to_it() {
        let rig = rig();
        rig.events
            .send(TurnEvent::Line(assistant_text(
                "the background task finished",
            )))
            .unwrap();
        rig.events.send(TurnEvent::Ended(costing(1.0))).unwrap();

        // The process answers the person only once it has been written to.
        let mut said = rig.said;
        let events = rig.events.clone();
        let responder = tokio::spawn(async move {
            let heard = said
                .recv()
                .await
                .expect("the person's message was never written");
            events
                .send(TurnEvent::Line(assistant_text("here is your answer")))
                .unwrap();
            events.send(TurnEvent::Ended(costing(2.0))).unwrap();
            heard
        });

        let transcript = std::sync::Arc::new(Mutex::new(String::new()));
        let mut live = rig.live;
        let served = tokio::time::timeout(
            Duration::from_secs(10),
            live.turn("and now me", &[], &transcript, None),
        )
        .await
        .expect("the turn must finish");

        let LiveTurn::Answered(outcome) = served else {
            panic!("the person's turn did not answer");
        };
        assert_eq!(
            outcome.cost_usd,
            Some(2.0),
            "the person got the other answer's numbers"
        );
        let stdout = transcript.lock().unwrap().clone();
        assert!(stdout.contains("here is your answer"));
        assert!(
            !stdout.contains("the background task finished"),
            "the spontaneous answer leaked into the person's turn"
        );
        assert_eq!(responder.await.unwrap().text, "and now me");

        let (kept_stdout, kept_outcome) = live
            .preempted
            .take()
            .expect("the answer set aside was not kept for the watcher");
        assert!(kept_stdout.contains("the background task finished"));
        assert_eq!(kept_outcome.cost_usd, Some(1.0));
    }

    /// A process that already stopped is not written to: the turn is as if it never happened, so it
    /// can be started fresh without running anything twice.
    #[tokio::test]
    async fn a_kept_process_that_already_stopped_is_not_written_to() {
        // Its supervisor has recorded why it ended, though its stdin is not yet seen closed.
        let mut rig = rig();
        rig.stopped
            .send(Some("killed by the tool policy".to_owned()))
            .unwrap();
        let transcript = std::sync::Arc::new(Mutex::new(String::new()));
        let served = rig.live.turn("hello?", &[], &transcript, None).await;
        assert!(matches!(served, LiveTurn::NotWritten));
        assert!(
            rig.said.try_recv().is_err(),
            "a process known to be dead was written to"
        );

        // Its stdin is closed: the runner's future, which owns the far end, is gone.
        let mut closed = self::rig();
        drop(closed.said);
        let transcript = std::sync::Arc::new(Mutex::new(String::new()));
        let served = closed.live.turn("hello?", &[], &transcript, None).await;
        assert!(matches!(served, LiveTurn::NotWritten));
    }

    /// A background task keeps a process alive past its idle window, but not forever: a task that
    /// never reports its end would otherwise pin a few hundred megabytes for good.
    #[tokio::test]
    async fn background_tasks_pin_a_process_for_at_most_two_hours() {
        assert_eq!(BACKGROUND_PIN_CAP, Duration::from_secs(2 * 60 * 60));
        let mut rig = rig();
        let started = Instant::now();
        rig.live.idle_since = started;
        rig.live.idle_for = LIVE_IDLE;
        rig.live.background.insert("task-1".to_owned());

        assert!(
            worth_keeping(&rig.live, None, started + Duration::from_secs(3_600)),
            "a task running for an hour must still pin its process"
        );
        assert!(
            !worth_keeping(
                &rig.live,
                None,
                started + BACKGROUND_PIN_CAP + Duration::from_secs(60)
            ),
            "a task running past the cap must stop pinning"
        );
        // The cap counts from the later of the turn's end and the last poll, like the idle window.
        let polled = started + Duration::from_secs(100 * 60);
        assert!(worth_keeping(
            &rig.live,
            Some(polled),
            started + Duration::from_secs(121 * 60)
        ));
    }
}
