import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import { routeTriple } from "../data/route";
import {
  RECORDED,
  runIsAlive,
  useCancelRun,
  useEndTurns,
  useReleaseWorktree,
  useRun,
  useRunBriefing,
  useRunStop,
  useRunTail,
  useSteerRun,
  type BriefingItem,
  type RunDetail as Run,
  type RunTailChunk,
  type StopDecision,
} from "../data/runs";
import {
  Badge,
  Button,
  ConfirmButton,
  ContextMeter,
  CostLine,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RunPipeline,
} from "../ui";
import { JudgeOpinionsBlock } from "./AutopilotJudge";
import { readRunStream, type RunEvent } from "../lib/run-stream";
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
        {run.isError ? <DetailError error={run.error} /> : <Quiet says={`reading run ${id}…`} />}
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
    <BriefingBlock key="briefing" id={id} />,
    <StdStreams key="streams" run={detail} />,
    /* Last, not beside StopBlock: the live-run reorder below lifts block 3 by position. */
    <JudgeOpinionsBlock key="judge" runId={id} />,
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

      {/* The shape first, then the figures. Which stage a run is standing at is what somebody
          opens the page to see, and it was previously assembled by reading a badge here, a
          second badge beside it and an exit code four blocks down. */}
      <RunPipeline run={detail} alive={alive} />

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
 * The two readings that are figures, on one line under the pipeline.
 *
 * It carried the run's state and its gate's as badges too, until `RunPipeline` above it started
 * drawing both as stages. Two readings of one state on one screen is worse than either alone:
 * they are a poll apart, and the moment they disagree a person has to work out which of them is
 * the app being slow. The states are the picture's; the figures are this row's.
 *
 * They were spread over a headline that said them as prose and a panel that said them
 * again as a definition list — "still going; $0.0310 spent; gate passed" above, a badge
 * and a bar below. Prose is the wrong shape for a figure that changes every three
 * seconds: it cannot be compared with the same figure on the run before it, and it puts
 * a number in the middle of a sentence where the eye has to parse to find it.
 *
 * So the figures are drawn as themselves, in one row, in the order the questions are
 * asked: what has it cost, and how full is it.
 */
function Instruments({ run }: { run: Run }) {
  return (
    <div className="mb-4">
      <div className="runs-gate-line">
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

      <RouteBlock run={run} />

      {/* What went into the prompt stays here and not in the strip above: it is read once, as a
          fact of how the run was built, not watched like the cost and the context fill. */}
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
      {run.chat_id != null && (
        <p className="runs-successor">
          This run is one turn of a conversation.{" "}
          <Link to={`/chats/${run.chat_id}`}>See the conversation</Link>.
        </p>
      )}
    </Panel>
  );
}

/** The `route_failed` JSON array as `a, b`; null when absent, empty or unreadable. */
function failedAttempts(raw: string | null | undefined): string | null {
  if (typeof raw !== "string" || raw === "") return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    const labels = parsed.filter((label): label is string => typeof label === "string" && label !== "");
    return labels.length === 0 ? null : labels.join(", ");
  } catch {
    return null;
  }
}

/**
 * What ran, and what the local router would have had it run.
 *
 * Renders nothing when the run carries none of it — a run from before the
 * router has no trail, and an empty section would claim there was one.
 * In `shadow` the advice is only a comparison, so it says whether it matched;
 * in `apply` the advice is what ran, and no comparison is drawn.
 */
function RouteBlock({ run }: { run: Run }) {
  const ran = routeTriple([run.runner, run.model, run.effort]);
  const advised = routeTriple([run.advised_runner, run.advised_model, run.advised_effort]);
  const mode = run.route_mode === "shadow" || run.route_mode === "apply" ? run.route_mode : null;
  const failed = failedAttempts(run.route_failed);
  // A mode with no decision is the router having given nothing usable: down, slow, refused, or an
  // answer outside what was asked. The run launched as it would have without it.
  const unadvised = mode !== null && !run.route_decision_id && advised === null;
  if (ran === null && advised === null && failed === null && !unadvised) return null;

  const matched =
    mode === "shadow" &&
    advised !== null &&
    run.advised_runner === run.runner &&
    run.advised_model === run.model &&
    run.advised_effort === run.effort;

  return (
    <div className="runs-route">
      {ran !== null && (
        <p className="runs-route-row">
          <span className="runs-route-label">Ran with</span> {ran}
        </p>
      )}
      {mode !== null && advised !== null && (
        <p className="runs-route-row">
          <span className="runs-route-label">Router advised</span> {advised}{" "}
          <Badge tone={mode === "apply" ? "active" : "shadow"}>{mode}</Badge>
          {mode === "shadow" && (
            <span className="runs-route-match">{matched ? "matched what ran" : "differs from what ran"}</span>
          )}
        </p>
      )}
      {unadvised && <p className="runs-successor">The router gave no usable advice; the run launched as configured.</p>}
      {failed !== null && <p className="runs-successor">{`Already failed on this item: ${failed}`}</p>}
    </div>
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

function BriefingBlock({ id }: { id: number }) {
  const { data, isError } = useRunBriefing(id);

  // Like the stop explanation, this is silent while loading or unreadable: it
  // explains a run that is already visible and must not become a second error.
  if (isError || !data || typeof data.traced !== "boolean") return null;

  let explanation: string | null = null;
  if (data.reason === "no_trace_context") {
    explanation =
      "This kind of run is briefed without a trace, so there is nothing to show here. That is not an empty briefing.";
  } else if (data.reason === "past_retention") {
    explanation =
      "The explanation of this briefing is past its retention window and has been deleted. What the run was told is not recoverable from here.";
  } else if (data.reason === "nothing_offered") {
    explanation =
      "Nothing was on offer for this run: no approved knowledge was in its scope when it started.";
  }

  return (
    <Panel title="What it was told">
      {explanation !== null ? (
        <p className="runs-briefing-meta">{explanation}</p>
      ) : data.traced ? (
        <>
          <BriefingList title="Shown to the run" items={data.items.filter((item) => item.shown)} />
          <BriefingList
            title="Offered and left out"
            items={data.items.filter((item) => !item.shown)}
          />
        </>
      ) : null}
    </Panel>
  );
}

function BriefingList({ title, items }: { title: string; items: BriefingItem[] }) {
  return (
    <section>
      <h3>{title}</h3>
      <ul>
        {items.map((item) => (
          <BriefingRow key={item.knowledge_id} item={item} />
        ))}
      </ul>
    </section>
  );
}

function BriefingRow({ item }: { item: BriefingItem }) {
  const [open, setOpen] = useState(false);
  const scope = item.scope_id ?? item.scope_kind;
  const signals = [
    ["text match", item.s_fts],
    ["similarity", item.s_sim],
    ["scope", item.s_scope],
    ["structure", item.s_structure],
    ["recency", item.s_recency],
    ["use", item.s_use],
  ] as const;

  return (
    <li className="runs-briefing-item">
      <p>{item.title}</p>
      <p className="runs-briefing-meta">{`${item.layer} · ${item.source} · ${scope}`}</p>
      <dl className="runs-briefing-signals">
        {signals.map(([label, value]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd>{value.toFixed(2)}</dd>
          </div>
        ))}
      </dl>
      <Button variant="quiet" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? "Hide body" : "Show body"}
      </Button>
      {open && <p>{item.body}</p>}
    </li>
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

  const [raw, setRaw] = useState(false);
  const events = useMemo(() => readRunStream(text), [text]);

  /**
   * Follows the bottom while the reader is at the bottom, and stays put once they scroll up to
   * read something: a well that yanks the page back down every poll cannot be read at all.
   */
  const well = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  useEffect(() => {
    const element = well.current;
    if (element !== null && pinned.current) element.scrollTop = element.scrollHeight;
  }, [text, raw]);
  const onScroll = () => {
    const element = well.current;
    if (element === null) return;
    pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 24;
  };

  return (
    <Panel
      title="Live output"
      aside={
        text !== "" && (
          <Button aria-pressed={raw} onClick={() => setRaw(!raw)}>
            {raw ? "Readable" : "Raw"}
          </Button>
        )
      }
    >
      {/* A read that failed says nothing about where the output is — unlike the
          204 below, which says exactly where it is. */}
      {tail.isError && <ErrorNote>the live tail is unreachable — the núcleo did not answer</ErrorNote>}
      {chunk === RECORDED && (
        <p className="runs-tail-note">
          recorded — this run has no live tail; what it wrote is in the stored output below
          {recorded === null ? ", and the daemon kept none of it" : ""}.
        </p>
      )}
      {text !== "" &&
        (raw ? (
          <div ref={well} onScroll={onScroll} className="runs-pre runs-tail">
            {text}
          </div>
        ) : (
          <div ref={well} onScroll={onScroll} className="runs-events-well">
            {events.length === 0 ? (
              <p className="runs-tail-note">nothing to read yet — the agent is working</p>
            ) : (
              <RunEventList events={events} />
            )}
          </div>
        ))}
      {text === "" && chunk !== RECORDED && !tail.isError && (
        <p className="runs-tail-note">{alive ? "nothing written yet" : "no live output was captured"}</p>
      )}
    </Panel>
  );
}

/** The small label in front of an event: who is speaking, or what kind of thing this is. */
function eventLabel(event: RunEvent): string {
  switch (event.kind) {
    case "said":
      return "agent";
    case "thought":
      return "thinking";
    case "tool":
      return event.name;
    case "result":
      return event.error ? "error" : "output";
    case "done":
      return "end";
    case "meta":
      return "run";
    case "raw":
      return "text";
  }
}

function RunEventList({ events }: { events: RunEvent[] }) {
  return (
    <ol className="runs-events">
      {events.map((event, at) => (
        <RunEventRow key={at} event={event} />
      ))}
    </ol>
  );
}

function RunEventRow({ event }: { event: RunEvent }) {
  const tone = event.kind === "result" || event.kind === "done" ? (event.error ? " runs-event-error" : "") : "";
  return (
    <li className={`runs-event runs-event-${event.kind}${tone}`}>
      <span className="runs-event-label">{eventLabel(event)}</span>
      <div className="runs-event-body">
        {event.kind === "tool" ? (
          event.detail !== "" && <code className="runs-event-code">{event.detail}</code>
        ) : event.kind === "result" || event.kind === "raw" ? (
          <pre className="runs-event-pre">{event.text}</pre>
        ) : (
          <p className="runs-event-text">{event.text}</p>
        )}
        {event.kind === "result" && event.more > 0 && (
          <span className="runs-event-more">
            … {event.more} more {event.more === 1 ? "line" : "lines"} — Raw shows them
          </span>
        )}
      </div>
    </li>
  );
}

/* -------------------------------------------------------------- streams -- */

/**
 * What the run wrote, in the end. Collapsed, because most of the time it is
 * hundreds of lines and the answer is the gate verdict above.
 */
function StdStreams({ run }: { run: Run }) {
  const [open, setOpen] = useState(false);
  const [raw, setRaw] = useState(false);
  // The same reading the live tail gets: a finished run's stdout is the stream-json it streamed.
  const events = useMemo(() => (run.stdout === null ? [] : readRunStream(run.stdout + "\n")), [run.stdout]);
  return (
    <Panel
      title="Stored output"
      aside={
        <>
          {open && run.stdout !== null && (
            <Button aria-pressed={raw} onClick={() => setRaw(!raw)}>
              {raw ? "Readable" : "Raw"}
            </Button>
          )}
          <Button aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? "Hide output" : "Show output"}
          </Button>
        </>
      }
    >
      {open ? (
        <>
          <h3 className="runs-stream-title">stdout</h3>
          {run.stdout === null ? (
            <pre className="runs-pre">nothing recorded</pre>
          ) : raw ? (
            <pre className="runs-pre">{run.stdout}</pre>
          ) : (
            <div className="runs-events-well">
              {events.length === 0 ? (
                <p className="runs-tail-note">nothing a person reads — Raw shows the wire</p>
              ) : (
                <RunEventList events={events} />
              )}
            </div>
          )}
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
 * and a semicolon list of three states is a table somebody typed out. Terminal states use the
 * badge below names the state; only a live run needs the prose "still going". What is left
 * is the one thing that is genuinely prose — what this run was for, and where it ran.
 */
function headline(run: Run): string {
  // The badge below already names the state. A terminal headline says where it ran; only a live
  // run needs a verb, because that fact is not visible at a glance in the badge's word.
  const where = run.project_id === null ? "outside any project" : `in ${run.project_id}`;
  return runIsAlive(run.status) ? `still going, ${where}` : `ran ${where}`;
}
