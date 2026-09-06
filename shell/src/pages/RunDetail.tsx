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

function UnknownRun({ raw }: { raw: string | undefined }) {
  return (
    <>
      <PageHeader title="Run" />
      <ErrorNote>
        <code>{raw ?? "(nothing)"}</code> is not a run id — runs are numbered.
      </ErrorNote>
      <Link to="/runs">Back to the index</Link>
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
        <PageHeader title={`Run ${id}`} />
        {run.isError ? <DetailError error={run.error} /> : <p className="runs-loading">reading run {id}…</p>}
        <Link to="/runs">Back to the index</Link>
      </>
    );
  }

  const alive = runIsAlive(detail.status);

  return (
    <>
      <PageHeader
        title={`Run ${id}`}
        headline={headline(detail)}
        actions={
          <div className="runs-detail-actions">
            {alive && (
              <ConfirmButton
                label="Cancel run"
                confirmLabel="Cancel it now"
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
                onConfirm={() => release.mutate()}
              />
            )}
          </div>
        }
      />

      {cancel.isError && <MutationNote error={cancel.error} what="that run could not be cancelled" />}
      {release.isError && <MutationNote error={release.error} what="that worktree could not be released" />}

      <FactsPanel run={detail} />
      <GateBlock run={detail} />
      {/* After the deterministic gate and before the output, which is the order
          a person reads them in: what the suite said, then why the run ended,
          then what it printed on the way. */}
      <StopBlock id={id} alive={alive} />
      <RunTail id={id} alive={alive} recorded={detail.stdout} />
      <StdStreams run={detail} />
      {/* Absent, not disabled. `steerable` was decided when the run was created
          and cannot change, so nothing a person could do here would make this
          run listen — a greyed-out box would be an invitation to try. */}
      {detail.steerable && <SteeringBox id={id} running={detail.status === "running"} />}

      <Link to="/runs">Back to the index</Link>
    </>
  );
}

/* ---------------------------------------------------------------- facts -- */

function FactsPanel({ run }: { run: Run }) {
  return (
    <Panel title="This run">
      <dl className="runs-facts">
        <Fact label="Status">
          <StateBadge domain="run" state={run.status} />
        </Fact>
        <Fact label="Project">{run.project_id ?? "no project"}</Fact>
        <Fact label="Session">{run.session_id ?? "none recorded"}</Fact>
        {/* Absent is not zero. A run killed before it reported has no exit code,
            which is a different fact from exiting 0. */}
        <Fact label="Exit code">{run.exit_code === null ? "none recorded" : String(run.exit_code)}</Fact>
        <Fact label="Turns">{run.num_turns === null ? "none recorded" : String(run.num_turns)}</Fact>
        <Fact label="Steerable">{run.steerable ? "yes — it accepts more turns" : "no"}</Fact>
      </dl>

      <CostLine
        costUsd={run.cost_usd}
        inputTokens={run.input_tokens}
        outputTokens={run.output_tokens}
        cachedTokens={run.cache_read_tokens}
      />
      <ContextMeter fill={run.context_fill} />
      <PromptBudget authored={run.authored_prompt_estimate} cliOwn={run.cli_own_estimate} />

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

/**
 * Where this run's prompt came from: the part we wrote, and everything else.
 *
 * **Two numbers, and there will never be a third.** The daemon knows exactly
 * what it put on the command line — the tool schemas its MCP server announced,
 * the standing instructions it appended, the helper definitions, the prompt —
 * and it knows the total the CLI reported. It does not know, and cannot find
 * out, how the remainder divides. The CLI's own system prompt, its built-in
 * tool definitions and whatever it loaded from a CLAUDE.md are all inside the
 * residual, and the daemon never sends the last of those, so no honest number
 * for it exists on this side. A third line here naming one of them would be a
 * guess wearing a measurement's clothes.
 *
 * Both are labelled *estimate* and neither is labelled *tokens*, because both
 * are four characters to the token — right about the order of magnitude and
 * wrong by tens of percent about anything finer.
 *
 * Absent is not zero, twice over. A run this daemon did not build a command
 * line for recorded nothing, and says so; a run that recorded what it wrote but
 * reported no usage has nothing to subtract from, so it shows what we wrote and
 * says the rest is unknown rather than showing a residual of zero — which would
 * read as *the CLI added nothing*, the exact opposite of the truth.
 */
function PromptBudget({ authored, cliOwn }: { authored: number | null; cliOwn: number | null }) {
  // Grouped for readability, and grouped by a NAMED locale rather than the
  // host's. A bare `toLocaleString()` follows whatever ICU locale the machine
  // happens to have, which rendered this `10 500` — narrow no-break space —
  // under the test runner's own. The shell is English throughout, so the
  // separator is a fact about this page and not about the machine showing it.
  const grouped = (value: number) => value.toLocaleString("en-US");
  if (authored === null) {
    return <p className="runs-successor">This run did not record what went into its prompt.</p>;
  }
  return (
    <>
      <p className="ui-cost">
        {/* One template literal rather than an interpolation between two text
            nodes: React would render three children, and a reading split across
            three nodes is one no test — and no screen reader — can match as the
            sentence it is. */}
        <span className="ui-cost-money">{`≈ ${grouped(authored)} estimate — what we wrote`}</span>
        <span className="ui-cost-tokens">
          {cliOwn === null ? "the rest is unknown" : `≈ ${grouped(cliOwn)} estimate — the CLI's own`}
        </span>
      </p>
      <p className="runs-successor">
        {cliOwn === null
          ? "This run reported no token usage, so there is nothing to subtract our share from."
          : "Tool schemas, standing instructions, helpers and the prompt, against everything else the CLI sent — undivided, because nothing here can tell its parts apart."}
      </p>
    </>
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
      <p className="runs-gate-line">
        <StateBadge domain="gate" state={run.gate_status} />
        {measured && (
          <span className="runs-gate-exit">
            {run.gate_exit_code === null ? "no exit code recorded" : `exit ${run.gate_exit_code}`}
          </span>
        )}
      </p>
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

/** One derived sentence about where this run got to. */
function headline(run: Run): string {
  const state = runIsAlive(run.status) ? "still going" : `ended ${run.status}`;
  const spend = run.cost_usd === null ? "no cost recorded" : `$ ${run.cost_usd.toFixed(4)} spent`;
  const gate =
    run.gate_status === null || run.gate_status.trim() === ""
      ? "no gate measured it"
      : `gate ${run.gate_status}`;
  return `${state}; ${spend}; ${gate}`;
}
