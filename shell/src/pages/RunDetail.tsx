import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  RECORDED,
  runIsAlive,
  useCancelRun,
  useEndTurns,
  useReleaseWorktree,
  useRun,
  useRunStop,
  useRunTail,
  useSteerRun,
  type RunDetail as Run,
  type RunTailChunk,
  type StopDecision,
} from "../data/runs";
import {
  Button,
  ConfirmButton,
  ContextMeter,
  CostLine,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  StateBadge,
} from "../ui";
import { readState } from "../ui/state-map";
import "./runs.css";

/**
 * One run, in full.
 *
 * Four blocks, in the order a person reads them: what this run is, what the
 * gate said about it, what it is writing right now, and what it wrote in the
 * end. The composer is fifth and only exists for a run that opted in.
 *
 * Nothing on this page renders the run's output as markup. Every stream here is
 * text a model wrote, and text a model wrote is untrusted input — there is no
 * `dangerouslySetInnerHTML` on this page and there must never be one.
 */
export function RunDetail() {
  const params = useParams({ strict: false }) as { runId?: string };
  const id = Number(params.runId);
  const valid = Number.isSafeInteger(id) && id > 0;

  if (!valid) return <UnknownRun raw={params.runId} />;
  return <KnownRun id={id} />;
}

/**
 * The way back to the index, above the title.
 *
 * "Back to the index" was the last line of the page, under the stored output — so the
 * way out was reachable only by scrolling past everything somebody had come to read,
 * and it was drawn three times over the page's three states. One crumb, at the top,
 * where a person looks when they realise they are in the wrong run.
 */
function Crumb() {
  return (
    <p className="mb-2 text-xs">
      <Link to="/runs">Runs</Link>
    </p>
  );
}

function UnknownRun({ raw }: { raw: string | undefined }) {
  return (
    <>
      <Crumb />
      <PageHeader title="Run" />
      <ErrorNote>
        <code>{raw ?? "(nothing)"}</code> is not a run id — runs are numbered.
      </ErrorNote>
    </>
  );
}

function KnownRun({ id }: { id: number }) {
  const run = useRun(id);
  const cancel = useCancelRun();
  const release = useReleaseWorktree(id);
  const detail = run.data;

  if (detail === undefined) {
    return (
      <>
        <Crumb />
        <PageHeader title={`Run ${id}`} />
        {run.isError ? <DetailError error={run.error} /> : <p className="runs-loading">reading run {id}…</p>}
      </>
    );
  }

  const alive = runIsAlive(detail.status);

  /*
    Keyed, and in an array, because the order changes with `alive` and `RunTail` holds
    the text it has accumulated in state. Reordered as bare JSX, React would reconcile
    by position: the moment a run ended, the tail would unmount and everything a person
    was reading would be replaced by "recorded — this run has no live tail". With keys
    it moves and keeps what it has.
  */
  const blocks = [
    <FactsPanel key="facts" run={detail} />,
    <GateBlock key="gate" run={detail} />,
    /* After the deterministic gate and before the output, which is the order a person
       reads them in: what the suite said, then why the run ended, then what it printed
       on the way. */
    <StopBlock key="stop" id={id} alive={alive} />,
    <RunTail key="tail" id={id} alive={alive} recorded={detail.stdout} />,
    <StdStreams key="streams" run={detail} />,
  ];
  /* While the run is going, what it is writing right now is the only block on this page
     that is changing, and it was fourth. A live run is watched, not read. */
  if (alive) blocks.unshift(...blocks.splice(3, 1));

  return (
    <>
      <Crumb />
      <PageHeader
        title={`Run ${id}`}
        headline={headline(detail)}
        actions={
          <div className="runs-detail-actions">
            {alive && (
              <ConfirmButton
                label="Cancel run"
                confirmLabel="Cancel it now"
                variant="ghost"
                intent="stop"
                onConfirm={() => cancel.mutate(id)}
              />
            )}
            {/* A worktree is only ever released from the pause: at any other
                moment the tree is still being worked in, and the daemon says so
                with a 409 rather than taking the work with it. */}
            {detail.status === "awaiting_approval" && (
              <ConfirmButton
                label="Release worktree"
                confirmLabel="Give the tree back"
                variant="quiet"
                onConfirm={() => release.mutate()}
              />
            )}
          </div>
        }
      />

      {cancel.isError && <MutationNote error={cancel.error} what="that run could not be cancelled" />}
      {release.isError && <MutationNote error={release.error} what="that worktree could not be released" />}

      <Instruments run={detail} />

      {blocks}

      {/* Absent, not disabled. `steerable` was decided when the run was created
          and cannot change, so nothing a person could do here would make this
          run listen — a greyed-out box would be an invitation to try. */}
      {detail.steerable && <SteeringBox id={id} running={detail.status === "running"} />}
    </>
  );
}

/* ----------------------------------------------------------- instruments -- */

/**
 * The four readings of a run, on one line under the header.
 *
 * They were spread over a headline that said them as prose and a panel that said them
 * again as a definition list — "still going; $0.0310 spent; gate passed" above, a badge
 * and a bar below. Prose is the wrong shape for a figure that changes every three
 * seconds: it cannot be compared with the same figure on the run before it, and it puts
 * a number in the middle of a sentence where the eye has to parse to find it.
 *
 * So the figures are drawn as themselves, in one row, in the order the questions are
 * asked: is it going, did the gate pass, what has it cost, how full is it.
 */
function Instruments({ run }: { run: Run }) {
  return (
    <div className="mb-4">
      <div className="runs-gate-line">
        <StateBadge domain="run" state={run.status} />
        <StateBadge domain="gate" state={run.gate_status} />
        <CostLine
          costUsd={run.cost_usd}
          inputTokens={run.input_tokens}
          outputTokens={run.output_tokens}
          cachedTokens={run.cache_read_tokens}
        />
        {/* A bar needs a width to be a bar. It takes the slack rather than a fixed
            column, so on a wide window it is a readable gauge and on a narrow one it
            wraps whole instead of collapsing to a dash. */}
        <div className="min-w-[14rem] flex-1">
          <ContextMeter fill={run.context_fill} />
        </div>
      </div>
    </div>
  );
}

/* ---------------------------------------------------------------- facts -- */

function FactsPanel({ run }: { run: Run }) {
  return (
    <Panel title="This run">
      {/* Status, cost and context fill were here and are now the strip under the header:
          they are the readings somebody arrives asking for, and they were four scrolls
          into a definition list. What is left is what a definition list is for — the
          fixed facts of a run, read once. */}
      <dl className="runs-facts">
        <Fact label="Project">{run.project_id ?? "no project"}</Fact>
        <Fact label="Session">{run.session_id ?? "none recorded"}</Fact>
        {/* Absent is not zero. A run killed before it reported has no exit code,
            which is a different fact from exiting 0. */}
        <Fact label="Exit code">{run.exit_code === null ? "none recorded" : String(run.exit_code)}</Fact>
        <Fact label="Turns">{run.num_turns === null ? "none recorded" : String(run.num_turns)}</Fact>
        <Fact label="Steerable">{run.steerable ? "yes — it accepts more turns" : "no"}</Fact>
      </dl>

      {/*
        The handoff link. The núcleo has recorded it since handoffs existed and
        the old shell never had the field, so a run that ran out of context and
        continued somewhere else looked, on screen, like a run that simply
        stopped — the continuation was reachable only by reading the database.
      */}
      {run.successor_run_id !== null && (
        <p className="runs-successor">
          This run handed its context on.{" "}
          <Link to={`/runs/${run.successor_run_id}`}>Run {run.successor_run_id} continued it</Link>.
        </p>
      )}
    </Panel>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="runs-fact">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/* ----------------------------------------------------------------- gate -- */

/**
 * What the gate said — three outcomes out of three, and a fourth for *nobody
 * asked*.
 *
 * A NULL `gate_status` is **no gate configured**, and it is rendered through
 * the same table every other state on this page goes through, so it cannot
 * quietly become a red pill. The distinction matters most for the person who
 * never wrote a gate: a UI that shows their absent test suite as a failure is
 * telling them their code is broken on the evidence of nothing at all.
 */
function GateBlock({ run }: { run: Run }) {
  const measured = run.gate_status !== null && run.gate_status.trim() !== "";
  return (
    <Panel title="Gate">
      {/* The VERDICT is the second instrument in the strip above — it is a reading, and a
          reading belongs where the other three are. What stays here is the evidence
          behind it, which is not a badge: the exit code, the sentence for a run nothing
          measured, and whatever the suite printed. */}
      {measured && (
        <p className="runs-gate-line">
          <span className="runs-gate-exit">
            {run.gate_exit_code === null ? "no exit code recorded" : `exit ${run.gate_exit_code}`}
          </span>
        </p>
      )}
      {!measured && (
        <p className="runs-gate-note">
          Nothing measured this run. There is no gate configured for it, so there is nothing that
          could have passed or failed.
        </p>
      )}
      {run.gate_output !== null && run.gate_output !== "" && (
        <pre className="runs-pre">{run.gate_output}</pre>
      )}
    </Panel>
  );
}

/* --------------------------------------------------------- why it stopped -- */

/**
 * Why the run stopped, which is a different question from the one the panel
 * above answers.
 *
 * **The two titles carry the whole distinction and neither may say just "gate".**
 * "Gate" above is the DETERMINISTIC gate: a test suite that ran and returned an
 * exit code, a verdict about the code. This is the TOOL gate: the classifier
 * deciding, call by call, whether the run was allowed to do a thing — and its
 * `kind: "gate"` means the run stopped waiting for a person to say yes. Two
 * mechanisms, one word, and a reader who conflates them draws exactly the wrong
 * conclusion about why their night ended.
 *
 * Nothing is rendered from `leading_up` for a kind that does not carry it: the
 * daemon sends `null` there rather than omitting the key, so an empty list and
 * "not applicable" stay distinguishable.
 */
function StopBlock({ id, alive }: { id: number; alive: boolean }) {
  const { data, isError } = useRunStop(id, alive);

  // Silent while it has not arrived and silent if it fails. This block is an
  // explanation of something already visible above, so a spinner or an error
  // strip here would be a second failure report about a page that is fine.
  if (isError || !data) return null;

  return (
    <Panel title="Why it stopped">
      <p className="runs-gate-line">{data.summary}</p>

      {/* §5.3. An empty list is the same shape whether the mode records nothing
          or the run simply asked for nothing, and only one of those is worth a
          person's time. */}
      {!data.decisions_recorded && (
        <p className="runs-gate-note">
          This run was watched by the person who started it, so the tool gate recorded no decisions
          for it. Nothing is missing.
        </p>
      )}

      {data.timeout !== null && (
        <p className="runs-gate-note">
          {`Ran ${minutes(data.timeout.elapsed_seconds)} against a ${minutes(
            data.timeout.silence_ceiling_seconds,
          )} silence ceiling and a ${minutes(data.timeout.wall_ceiling_seconds)} wall clock. `}
          {data.timeout.verdict === "silence"
            ? "It went quiet: the wall clock was nowhere near, so what ran out was patience with a run that had stopped reporting."
            : "Which of the two fired cannot be told apart from here, because the elapsed time is measured from when the run was created rather than from when it began."}
        </p>
      )}

      {data.gate !== null && <StopDecisionLine decision={data.gate} highlight />}

      {data.exit_code !== null && <p className="runs-gate-note">{`Exit code ${data.exit_code}.`}</p>}
      {data.stderr_tail !== null && data.stderr_tail !== "" && (
        <pre className="runs-pre">{data.stderr_tail}</pre>
      )}
      {data.successor_run_id !== null && (
        <p className="runs-gate-note">
          <Link to="/runs/$runId" params={{ runId: String(data.successor_run_id) }}>
            {`Continued as run ${data.successor_run_id}`}
          </Link>
        </p>
      )}

      {data.leading_up !== null && data.leading_up.length > 0 && (
        <>
          <p className="runs-gate-note">What it did just before, most recent first:</p>
          {data.leading_up.map((decision, index) => (
            <StopDecisionLine key={`${decision.created_at}-${index}`} decision={decision} />
          ))}
        </>
      )}
    </Panel>
  );
}

/**
 * One decision, as a line.
 *
 * `tool_input_truncated` is rendered as words rather than as an ellipsis glued
 * to the text, for the reason the field exists at all: a command that genuinely
 * ends in `...` and one that was cut must not look the same.
 */
function StopDecisionLine({ decision, highlight }: { decision: StopDecision; highlight?: boolean }) {
  return (
    <div className={highlight ? "runs-gate-line" : "runs-gate-note"}>
      <span>{`${decision.tool_name} — ${decision.decision} (${decision.action_class})`}</span>
      {decision.reason !== null && <span>{` ${decision.reason}`}</span>}
      {decision.tool_input !== null && decision.tool_input !== "" && (
        <pre className="runs-pre">{decision.tool_input}</pre>
      )}
      {decision.tool_input_truncated && (
        <span className="runs-gate-note">cut for length; the daemon holds the rest</span>
      )}
    </div>
  );
}

/** Seconds as a person reads them. Whole minutes: nothing here turns on a second. */
function minutes(seconds: number): string {
  const whole = Math.round(seconds / 60);
  return whole === 1 ? "1 minute" : `${whole} minutes`;
}

/* ----------------------------------------------------------------- tail -- */

/**
 * The live tail, advanced by the daemon's byte cursor.
 *
 * `next` is handed straight back on the following request. Recomputing the
 * offset from the length of the received string would drift on the first
 * non-ASCII byte the run writes — JavaScript counts UTF-16 code units and the
 * daemon counts bytes — and from that point on the tail would redraw text that
 * is already on screen.
 */
function RunTail({ id, alive, recorded }: { id: number; alive: boolean; recorded: string | null }) {
  const [since, setSince] = useState(0);
  const [text, setText] = useState("");
  const tail = useRunTail(id, since, alive);
  const chunk = tail.data;

  /**
   * Consumed by object identity, not by offset.
   *
   * react-query keeps the previous object when a refetch is deep-equal, so an
   * unchanged tail simply does not re-run this. The ref is what makes a second
   * invocation of the *same* chunk — a StrictMode remount in development — a
   * no-op rather than a duplicated paragraph.
   */
  const consumed = useRef<RunTailChunk | null>(null);
  useEffect(() => {
    if (chunk === undefined || chunk === RECORDED) return;
    if (consumed.current === chunk) return;
    consumed.current = chunk;
    if (chunk.text === "") return;
    setText((current) => current + chunk.text);
    setSince(chunk.next);
  }, [chunk]);

  return (
    <Panel title="Live output">
      {/* A read that failed says nothing about where the output is — unlike the
          204 below, which says exactly where it is. */}
      {tail.isError && <ErrorNote>the live tail is unreachable — the núcleo did not answer</ErrorNote>}
      {chunk === RECORDED && (
        <p className="runs-tail-note">
          recorded — this run has no live tail; what it wrote is in the stored output below
          {recorded === null ? ", and the daemon kept none of it" : ""}.
        </p>
      )}
      {text !== "" && <pre className="runs-pre runs-tail">{text}</pre>}
      {text === "" && chunk !== RECORDED && !tail.isError && (
        <p className="runs-tail-note">{alive ? "nothing written yet" : "no live output was captured"}</p>
      )}
    </Panel>
  );
}

/* -------------------------------------------------------------- streams -- */

/**
 * What the run wrote, in the end. Collapsed, because most of the time it is
 * hundreds of lines and the answer is the gate verdict above.
 */
function StdStreams({ run }: { run: Run }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      title="Stored output"
      aside={
        <Button aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "Hide output" : "Show output"}
        </Button>
      }
    >
      {open ? (
        <>
          <h3 className="runs-stream-title">stdout</h3>
          <pre className="runs-pre">{run.stdout ?? "nothing recorded"}</pre>
          <h3 className="runs-stream-title">stderr</h3>
          <pre className="runs-pre">{run.stderr ?? "nothing recorded"}</pre>
        </>
      ) : (
        <p className="runs-tail-note">
          {run.stdout === null && run.stderr === null
            ? "nothing was recorded for this run"
            : "hidden — this is usually long, and the gate above is the verdict"}
        </p>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------- steering -- */

/**
 * Say another turn to a run that is listening.
 *
 * The *end the turns* control is beside the composer rather than hidden behind
 * a menu, because without it a steerable run cannot finish at all: it reads
 * until its stdin closes, and a conversation left open trips the progress
 * deadline and is recorded `timed_out` — a failure status, for having waited.
 */
function SteeringBox({ id, running }: { id: number; running: boolean }) {
  const steer = useSteerRun(id);
  const end = useEndTurns(id);
  const [message, setMessage] = useState("");

  return (
    <Panel title="Speak to this run">
      <form
        className="runs-steer"
        onSubmit={(event) => {
          event.preventDefault();
          if (message.trim() === "" || steer.isPending) return;
          steer.mutate(message.trim(), { onSuccess: () => setMessage("") });
        }}
      >
        <label className="runs-field">
          <span>Next turn</span>
          <textarea
            rows={3}
            value={message}
            aria-label="Say something to this run"
            onChange={(event) => setMessage(event.target.value)}
          />
        </label>
        <div className="runs-steer-actions">
          <Button type="submit" intent="go" disabled={!running || steer.isPending}>
            Send turn
          </Button>
          <ConfirmButton
            label="End the turns"
            confirmLabel="Close its input"
            variant="danger"
            disabled={end.isPending}
            onConfirm={() => end.mutate()}
          />
        </div>
        {!running && (
          <p className="runs-tail-note">
            this run is no longer running, so it has no channel left to reach — ending the turns is
            still safe and still the way to close one
          </p>
        )}
        {steer.isError && <SteerRefusal error={steer.error} />}
        {end.isError && <MutationNote error={end.error} what="its turns could not be closed" />}
      </form>
    </Panel>
  );
}

/**
 * Why the run would not take the turn.
 *
 * The two refusals are not interchangeable and a person can act on the
 * difference: 409 is *this run is not listening* — it ended, or the channel is
 * gone — and 403 is *this run may never be spoken to*, which is a property of
 * the mode it was launched in and will not change.
 */
function SteerRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the turn was not delivered</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: "this run is not listening any more — it has ended, or its channel is already closed",
        forbidden: "this run may never be spoken to: the mode it was launched in has no tools to steer",
        not_found: "there is no such run — it may have been swept",
      }}
    />
  );
}

/* --------------------------------------------------------------- notes -- */

function DetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "there is no run with that number", ...daemonProse(error) }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this run</ErrorNote>;
}

function MutationNote({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}

function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const prose = refusal.detail.trim();
  return prose === "" || prose === refusal.code ? {} : { [refusal.code]: prose };
}

/**
 * One derived sentence about where this run got to.
 *
 * **No figures.** It used to read "still going; $0.0310 spent; gate passed", and all
 * three of those are readings that belong in the strip below it, drawn as themselves:
 * a spend written into a sentence cannot be compared with the spend on the run before,
 * and a semicolon list of three states is a table somebody typed out. What is left is
 * the one thing that is genuinely prose — what this run was for, and where it ran.
 */
function headline(run: Run): string {
  const state = runIsAlive(run.status)
    ? "still going"
    : `ended ${readState("run", run.status)?.label ?? run.status}`;
  const where = run.project_id === null ? "no project" : `in ${run.project_id}`;
  return `${state}, ${where}`;
}
