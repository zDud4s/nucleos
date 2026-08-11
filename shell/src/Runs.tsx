import { useCallback, useEffect, useRef, useState } from "react";
import {
  cancelRun, createRun, endRunTurns, getRun, getRuns, releaseWorktree, steerRun,
  RUN_MODES, RUN_MODE_FILTERS, RUN_STATUSES,
  type ConnectionState, type RunDetail, type RunSearchResult, type RunsFilter,
} from "./api";
import {
  CONTEXT_WINDOW_TOKENS, HANDOFF_FRACTION, contextPressure,
  formatTokens, formatUsd, gateTone, relativeTime, runIsLive, runStatusLabel, runTone,
} from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";
import Presets from "./Presets";

/** How many rows the list asks for. The daemon caps this; this is the shell's own default. */
const DEFAULT_LIMIT = 50;

function orNull(value: string): string | null {
  return value.trim() === "" ? null : value.trim();
}

interface NewRunProps {
  token: string;
  onStarted: (runId: number) => void;
}

/**
 * The hand-written run.
 *
 * This is a person-initiated start, which is the one path that still goes through the daemon's
 * `create_run` front door — so it inherits the kill switch and the budget pause, and both come back
 * as refusals with their own status. Those are named rather than collapsed: "it didn't work" would
 * send someone hunting for an outage when the answer is a switch they themselves engaged.
 */
function NewRun({ token, onStarted }: NewRunProps) {
  const [prompt, setPrompt] = useState("");
  const [projectId, setProjectId] = useState("");
  const [cwd, setCwd] = useState("");
  const [mode, setMode] = useState<string>("real");
  const [steerable, setSteerable] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  async function start() {
    setBusy(true);
    setFailed(null);
    setNote(null);
    const result = await createRun(token, {
      prompt: prompt.trim(),
      project_id: orNull(projectId),
      cwd: orNull(cwd),
      mode,
      steerable,
    });
    setBusy(false);
    if (!result.ok) {
      setFailed(
        result.status === 503
          ? "Refused: the kill switch is engaged. Disengage it to start work again."
          : result.status === 429
            ? "Refused: autopilot is paused by budget. Approvals still work."
            : result.status === 400
              ? "The daemon rejected this request — check the project and working directory."
              : "Could not start the run.",
      );
      return;
    }
    setNote(`Run #${result.value} started.`);
    setPrompt("");
    onStarted(result.value);
  }

  return (
    <Panel title="Start a run" aside="goes through the same door as autopilot's">
      <form
        className="form-grid"
        onSubmit={(event) => {
          event.preventDefault();
          if (prompt.trim() === "" || busy) return;
          void start();
        }}
      >
        <label>
          Mode
          <select value={mode} onChange={(event) => setMode(event.target.value)}>
            {RUN_MODES.map((option) => <option key={option} value={option}>{option}</option>)}
          </select>
        </label>
        <label>
          Project
          <input
            value={projectId}
            placeholder="(none)"
            onChange={(event) => setProjectId(event.target.value)}
          />
        </label>
        <label>
          Working directory
          <input
            value={cwd}
            placeholder="(the project root)"
            onChange={(event) => setCwd(event.target.value)}
          />
        </label>
        <label className="wide">
          Prompt
          <textarea
            rows={4}
            value={prompt}
            placeholder="What should it do?"
            onChange={(event) => setPrompt(event.target.value)}
          />
        </label>
        <label className="wide check">
          <input
            type="checkbox"
            checked={steerable}
            onChange={(event) => setSteerable(event.target.checked)}
          />
          <span>
            Let me talk to it while it works
            {/* The consequence belongs next to the box, not in a tooltip: a listening run does not
                end by itself. It reads turns until its input closes, so someone has to say the
                conversation is over — and a run left listening is recorded as having timed out,
                which reads afterwards as a failure rather than as a run nobody dismissed. */}
            <em>
              A run that listens keeps waiting for your next message. End the conversation from the
              run itself when you are done, or it will sit there until its deadline.
            </em>
          </span>
        </label>
        <div className="form-actions">
          <Button type="submit" variant="approve" disabled={prompt.trim() === "" || busy}>
            {busy ? "Starting…" : "Start run"}
          </Button>
          <span className="cta-note">
            {mode === "shadow"
              ? "Shadow: it decides but never acts."
              : mode === "worktree"
                ? "Worktree: it works on an isolated copy."
                : "Real: it acts on the project as it stands."}
          </span>
        </div>
      </form>
      {note !== null && <p className="gate-note">{note}</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </Panel>
  );
}

interface SteeringProps {
  token: string;
  runId: number;
  onEnded: () => void;
}

/**
 * Saying something to a run that is already working.
 *
 * Only drawn for a run that was created steerable and is still going — the daemon decides that when
 * the run starts and never revisits it, so an absent composer means this run has no ear rather than
 * that the moment has passed. A message is queued rather than delivered: the run picks it up when it
 * next reads its input, so the confirmation says accepted, not answered.
 */
function Steering({ token, runId, onEnded }: SteeringProps) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  async function say() {
    const message = text.trim();
    setBusy(true);
    setFailed(null);
    setNote(null);
    const result = await steerRun(token, runId, message);
    setBusy(false);
    if (!result.ok) {
      setFailed(
        // The two refusals are not the same thing and lead somewhere different: one is about this
        // moment, the other is about this run for as long as it exists.
        result.status === 409
          ? "This run is not listening any more — it finished, or it was never started to listen."
          : result.status === 403
            ? "This run may never be spoken to. It reads text nobody vouches for, so it takes no second author."
            : "The daemon did not take the message.",
      );
      return;
    }
    setText("");
    setNote("Queued — it reads this when it next comes up for air.");
  }

  async function end() {
    setBusy(true);
    setFailed(null);
    const closed = await endRunTurns(token, runId);
    setBusy(false);
    if (!closed) {
      setFailed("Could not close the conversation.");
      return;
    }
    setNote("Conversation closed. It finishes the turn it is on, then stops.");
    onEnded();
  }

  return (
    <form
      className="steer"
      onSubmit={(event) => {
        event.preventDefault();
        if (text.trim() === "" || busy) return;
        void say();
      }}
    >
      <textarea
        rows={2}
        value={text}
        placeholder="Say something to this run…"
        onChange={(event) => setText(event.target.value)}
      />
      <div className="a-actions">
        <Button type="submit" size="sm" variant="approve" disabled={text.trim() === "" || busy}>
          {busy ? "Sending…" : "Send"}
        </Button>
        {/* Confirmed, because this is the end of the conversation and there is no reopening it:
            the daemon lets go of the run's input, and a closed channel cannot be given back. */}
        <ConfirmButton
          size="sm"
          confirmLabel="Confirm end?"
          disabled={busy}
          onConfirm={() => void end()}
        >
          End the conversation
        </ConfirmButton>
      </div>
      {note !== null && <p className="gate-note">{note}</p>}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </form>
  );
}

interface RunDetailProps {
  token: string;
  runId: number;
  onCancelled: () => void;
}

/**
 * One run, opened.
 *
 * Polls only while the run is still moving and stops the moment it settles — a finished run's row
 * never changes again, so re-reading it every 3 seconds would be asking a question already answered.
 */
function RunDetailView({ token, runId, onCancelled }: RunDetailProps) {
  const [detail, setDetail] = useState<RunDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  // Read by the poll's closure, which would otherwise keep the status from the tick it was created.
  const live = useRef(true);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      const next = await getRun(token, runId);
      if (cancelled) return;
      setDetail(next);
      setLoading(false);
      live.current = next !== null && runIsLive(next.status);
    };
    void load();
    const id = setInterval(() => {
      if (live.current) void load();
    }, 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [runId, token]);

  async function stop() {
    setBusy(true);
    setFailed(null);
    const stopped = await cancelRun(token, runId);
    setBusy(false);
    if (!stopped.ok) {
      // 404 is the common case and is not really a failure: the run finished between the page
      // drawing the button and the click reaching the daemon.
      setFailed("Nothing to cancel — the run had already ended.");
    }
    onCancelled();
  }

  /**
   * Abandons a run parked for approval, and deletes the tree it was holding.
   *
   * The other door out of `awaiting_approval`, beside answering the proposal on the Autopilot tab.
   * Rejecting there refuses one request; this refuses the run — it cancels the row and removes the
   * worktree without an answer ever being given, which is what you want for a parked run whose
   * question you are not going to answer at all.
   *
   * The edits in that tree go with it. That is the point of the button rather than a caveat about
   * it: a run stuck at a proposal pins a worktree, and until now nothing in this window could let
   * one go — the daemon's GC does not touch a tree an `awaiting_approval` run still owns.
   */
  async function release() {
    setBusy(true);
    setFailed(null);
    const released = await releaseWorktree(token, runId);
    setBusy(false);
    if (!released) {
      // 409 means the run left `awaiting_approval` between the draw and the click — most often
      // because the proposal was answered elsewhere, which is the good ending, not a failure.
      setFailed("Nothing to release — this run is no longer waiting for approval.");
    }
    onCancelled();
  }

  if (loading) return <p className="a-note">Opening…</p>;
  if (detail === null) return <ErrorNote>Could not read this run.</ErrorNote>;

  const gate = gateTone(detail.gate_status);
  const pressure = contextPressure(detail.context_fill);
  const stillRunning = runIsLive(detail.status);
  // The one state the daemon accepts a release in; it answers 409 for every other.
  const parked = detail.status === "awaiting_approval";

  return (
    <div className="run-detail">
      <div className="rd-facts">
        <span>exit <b>{detail.exit_code ?? "—"}</b></span>
        <span>cost <b>{detail.cost_usd === null ? "—" : formatUsd(detail.cost_usd)}</b></span>
        <span>turns <b>{detail.num_turns ?? "—"}</b></span>
        <span>in <b>{formatTokens(detail.input_tokens)}</b></span>
        <span>out <b>{formatTokens(detail.output_tokens)}</b></span>
        <span>cached <b>{formatTokens(detail.cache_read_tokens)}</b></span>
        {detail.steerable && <Badge tone="active">listening</Badge>}
        {detail.session_id !== null && <span className="rd-session">{detail.session_id}</span>}
      </div>
      {pressure !== null && (
        <div className="rd-context">
          <div className="ctx-track">
            <div
              className={pressure.handingOff ? "ctx-fill at-limit" : "ctx-fill"}
              style={{ width: `${Math.round(pressure.fraction * 100)}%` }}
            />
            {/* The threshold is drawn on the track rather than described in words, because what the
                number means is entirely "how far is it from that line". */}
            <div className="ctx-mark" style={{ left: `${HANDOFF_FRACTION * 100}%` }} />
          </div>
          <span className="ctx-note">
            context <b>{formatTokens(pressure.fill)}</b> of {formatTokens(CONTEXT_WINDOW_TOKENS)}
            {pressure.handingOff
              ? " — past the handoff line, so this run continues in a fresh successor"
              : " · hands off to a successor at the mark"}
          </span>
        </div>
      )}
      {gate !== null && (
        <div className="rd-gate">
          <Badge tone={gate}>gate {detail.gate_status}</Badge>
          <span>exit {detail.gate_exit_code ?? "—"}</span>
          {detail.gate_output !== null && detail.gate_output !== "" && (
            <pre className="rd-stream">{detail.gate_output}</pre>
          )}
        </div>
      )}
      {/* Rendered as text, never as markup: this is a model's output and a subprocess's, and both
          are outside this machine's control in the same way an email body is. */}
      {detail.stdout !== null && detail.stdout !== "" && (
        <details className="rd-out">
          <summary>output</summary>
          <pre className="rd-stream">{detail.stdout}</pre>
        </details>
      )}
      {detail.stderr !== null && detail.stderr !== "" && (
        <details className="rd-out" open>
          <summary>errors</summary>
          <pre className="rd-stream">{detail.stderr}</pre>
        </details>
      )}
      {stillRunning && detail.steerable && (
        <Steering token={token} runId={runId} onEnded={onCancelled} />
      )}
      {stillRunning && (
        <div className="a-actions">
          <ConfirmButton
            size="sm"
            variant="danger"
            confirmLabel="Confirm cancel?"
            disabled={busy}
            onConfirm={() => void stop()}
          >
            Cancel run
          </ConfirmButton>
        </div>
      )}
      {parked && (
        <div className="rd-parked">
          <p className="a-note">
            Parked waiting for approval. Answering it is on the Autopilot tab; letting it go is here,
            and takes the worktree — and everything written in it — with it.
          </p>
          <div className="a-actions">
            <ConfirmButton
              size="sm"
              variant="danger"
              confirmLabel="Discard run and worktree?"
              disabled={busy}
              onConfirm={() => void release()}
            >
              Abandon and release worktree
            </ConfirmButton>
          </div>
        </div>
      )}
      {failed !== null && <p className="gate-note">{failed}</p>}
    </div>
  );
}

interface RunsProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * The run index: what the núcleo has done, and the two ways to ask it to do more.
 *
 * Its own tab rather than a panel in Autopilot, because Autopilot answers "what wants my signature
 * right now" and this answers "what happened". The approval queue there is a slice of this list; the
 * rest of it — everything already finished — had nowhere to be seen at all.
 */
function Runs({ token, connection }: RunsProps) {
  const unavailable = connection !== "connected" || token === null;
  const [runs, setRuns] = useState<RunSearchResult[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [openId, setOpenId] = useState<number | null>(null);
  const [projectId, setProjectId] = useState("");
  const [status, setStatus] = useState("");
  const [mode, setMode] = useState("");
  const [q, setQ] = useState("");
  const inFlight = useRef(false);

  const filter: RunsFilter = {
    projectId: projectId.trim() === "" ? undefined : projectId.trim(),
    status: status === "" ? undefined : status,
    mode: mode === "" ? undefined : mode,
    q: q.trim() === "" ? undefined : q.trim(),
    limit: DEFAULT_LIMIT,
  };
  // Serialised so the refresh callback depends on the VALUES rather than on a fresh object every
  // render, which would restart the interval on every keystroke.
  const filterKey = JSON.stringify(filter);

  const refresh = useCallback(
    async (background = false) => {
      if (token === null || connection !== "connected") return;
      // One round at a time: on a daemon slower than the tick, stacked rounds can land out of order
      // and settle the list on an older answer than it was already showing.
      if (background && inFlight.current) return;
      inFlight.current = true;
      if (!background) setLoading(true);
      try {
        const next = await getRuns(token, JSON.parse(filterKey) as RunsFilter);
        setRuns(next);
      } finally {
        inFlight.current = false;
        if (!background) setLoading(false);
      }
    },
    [connection, filterKey, token],
  );

  useEffect(() => {
    if (unavailable) {
      setRuns(null);
      setLoading(true);
      return;
    }
    void refresh();
  }, [refresh, unavailable]);

  useEffect(() => {
    if (unavailable) return;
    const id = setInterval(() => void refresh(true), 3000);
    return () => clearInterval(id);
  }, [refresh, unavailable]);

  // Spelled out rather than reusing `unavailable`, because this is also what narrows `token` to a
  // string for everything below — a boolean derived elsewhere tells the compiler nothing.
  if (connection !== "connected" || token === null) {
    return (
      <section className="runs">
        <Teach title="Runs are waiting for the daemon.">
          Connect to the daemon to see what has run, and to start something new. Nothing was lost —
          the history lives in the núcleo, not here.
        </Teach>
      </section>
    );
  }

  const liveCount = (runs ?? []).filter((run) => runIsLive(run.status)).length;
  const waiting = (runs ?? []).filter((run) => run.status === "awaiting_approval").length;

  return (
    <section className="runs">
      <h1 className="headline">
        {liveCount > 0
          ? <><em>{liveCount} run{liveCount === 1 ? "" : "s"}</em> in flight.</>
          : waiting > 0
            ? <><em>{waiting}</em> waiting on your approval.</>
            : <>Nothing running. <span className="ok">The núcleo is idle.</span></>}
      </h1>
      <div className="statusline">
        <span>{runs?.length ?? 0} shown</span>
        <span>newest first · <b>up to {DEFAULT_LIMIT}</b></span>
      </div>
      <div className="grid">
        <div className="stack">
          <Panel
            title="History"
            aside={loading && runs === null ? "loading" : `${runs?.length ?? 0} runs`}
          >
            <div className="filters">
              <label>
                Project
                <input
                  value={projectId}
                  placeholder="any"
                  onChange={(event) => setProjectId(event.target.value)}
                />
              </label>
              <label>
                Status
                <select value={status} onChange={(event) => setStatus(event.target.value)}>
                  <option value="">any</option>
                  {RUN_STATUSES.map((option) => (
                    <option key={option} value={option}>{runStatusLabel(option)}</option>
                  ))}
                </select>
              </label>
              <label>
                Mode
                <select value={mode} onChange={(event) => setMode(event.target.value)}>
                  <option value="">any</option>
                  {RUN_MODE_FILTERS.map((option) => (
                    <option key={option} value={option}>{option}</option>
                  ))}
                </select>
              </label>
              <label className="wide">
                Contains
                <input
                  value={q}
                  placeholder="search the prompt"
                  onChange={(event) => setQ(event.target.value)}
                />
              </label>
            </div>
            {loading && runs === null && <p className="a-note">Loading…</p>}
            {!loading && runs === null && (
              <ErrorNote>Could not load runs from the daemon.</ErrorNote>
            )}
            {runs !== null && runs.length === 0 && (
              <Teach title="No runs match.">
                Every run the núcleo has taken lands here — yours, autopilot&apos;s, the
                scheduler&apos;s. Widen the filters, or start one below.
              </Teach>
            )}
            {(runs ?? []).map((run) => (
              <article className="feed-item" key={run.id}>
                <button
                  type="button"
                  className="mail-row"
                  aria-expanded={openId === run.id}
                  onClick={() => setOpenId((open) => (open === run.id ? null : run.id))}
                >
                  <div className="f-meta">
                    <span className="d-id">#{run.id}</span>
                    <Badge tone={runTone(run.status)}>{runStatusLabel(run.status)}</Badge>
                    <span className="p-mode">{run.mode}</span>
                    <span>{run.project_id ?? "no project"}</span>
                    <time dateTime={run.created_at} title={run.created_at}>
                      {relativeTime(run.created_at)}
                    </time>
                    <span>{run.cost_usd === null ? "—" : formatUsd(run.cost_usd)}</span>
                  </div>
                  <p className="f-body p-prompt">{run.prompt_excerpt}</p>
                </button>
                {openId === run.id && (
                  <RunDetailView
                    token={token}
                    runId={run.id}
                    onCancelled={() => void refresh(true)}
                  />
                )}
              </article>
            ))}
          </Panel>
        </div>
        <div className="stack">
          <NewRun
            token={token}
            onStarted={(runId) => { setOpenId(runId); void refresh(true); }}
          />
          <Presets
            token={token}
            onRunStarted={(runId) => { setOpenId(runId); void refresh(true); }}
          />
        </div>
      </div>
    </section>
  );
}

export default Runs;
