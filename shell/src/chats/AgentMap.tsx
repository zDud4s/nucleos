import { useMemo, useState } from "react";
import { useLiveTurnSnapshot, useChatTasks, useStopTask, useTurnTools, type ChatTask } from "../data/chats";
import { turnIsLive, type ToolCall, type Turn } from "../lib/turns";
import { displayName } from "../lib/modelName";
import { plural } from "../lib/plural";
import { useNow } from "../lib/useNow";
import { Modal } from "../ui/Modal";
import { Button } from "../ui/Button";
import { Quiet } from "../ui/Quiet";
import { tokenCount } from "../ui/readings";
import { cacheState, leftText } from "./cache";

/** A Task or Agent call is a subagent; nothing else spawns a child conversation. */
export function isSubagent(call: ToolCall): boolean {
  return call.name === "Task" || call.name === "Agent";
}

export interface AgentCard {
  /** Stable key: the tool_use id, or a positional fallback for a daemon that sends none. */
  key: string;
  kind: "subagent" | "background";
  call: ToolCall;
  working: boolean;
  /** The durable row, when the daemon has one; its word wins over the turn's snapshot. */
  task?: ChatTask;
  /** Calls this subagent made itself: those whose `parent` is its id. */
  calls: ToolCall[];
  /** The turn it ran under, whose tool results `useTurnTools` reads for the transcript. */
  turnId?: number;
}

export interface AgentGroups {
  subagents: AgentCard[];
  background: AgentCard[];
  running: number;
  total: number;
}

/**
 * Sort the conversation's calls into the map's two groups.
 *
 * Working means: on a live turn, a background task whose `status` is running (or, with no status,
 * no `finished_at`); a subagent with no `finished_at` on a live turn. A settled turn's agent is
 * working only when its durable row says `running`; without a row nothing the page can see
 * outlives its turn. A row with no call in the transcript still becomes a card.
 */
export function groupAgents(turns: Turn[] | undefined, tasks?: ChatTask[]): AgentGroups {
  const subagents: AgentCard[] = [];
  const background: AgentCard[] = [];
  const rows = new Map((tasks ?? []).map((t) => [t.tool_use_id, t]));
  const seen = new Set<string>();
  for (const turn of turns ?? []) {
    const live = turnIsLive(turn.status);
    turn.did.forEach((call, index) => {
      const key = call.id ?? `${turn.id}-${index}`;
      const sub = isSubagent(call);
      if (!sub && call.background !== true) return;
      const task = call.id !== undefined ? rows.get(call.id) : undefined;
      if (call.id !== undefined && task !== undefined) seen.add(call.id);
      const shown = task?.finished_at ? { ...call, finished_at: task.finished_at } : call;
      if (sub) {
        const calls = turn.did.filter(
          (c) => c.parent !== undefined && c.parent !== null && c.parent === call.id,
        );
        const working = task !== undefined ? task.status === "running" : live && !call.finished_at;
        subagents.push({ key, kind: "subagent", call: shown, working, task, calls, turnId: turn.id });
      } else {
        // Only a live turn's word is current: a settled turn's row was saved when the turn ended,
        // and a task still running then stays "running" in it forever.
        const working =
          task !== undefined
            ? task.status === "running"
            : live && (call.status !== undefined ? call.status === "running" : !call.finished_at);
        background.push({ key, kind: "background", call: shown, working, task, calls: [], turnId: turn.id });
      }
    });
  }
  for (const t of tasks ?? []) {
    if (seen.has(t.tool_use_id)) continue;
    const call: ToolCall = {
      name: t.kind === "background_bash" ? "Bash" : "Agent",
      detail: null,
      todos: [],
      id: t.tool_use_id,
      subagent_type: t.subagent_type ?? undefined,
      model: t.model ?? undefined,
      background: t.kind !== "subagent",
      started_at: t.started_at,
      finished_at: t.finished_at ?? undefined,
      tokens: t.total_tokens ?? undefined,
    };
    const card: AgentCard = {
      key: t.tool_use_id,
      kind: t.kind === "background_bash" ? "background" : "subagent",
      call,
      working: t.status === "running",
      calls: [],
      turnId: t.launched_by_run_id,
      task: t,
    };
    (card.kind === "subagent" ? subagents : background).push(card);
  }
  const running = [...subagents, ...background].filter((c) => c.working).length;
  return { subagents, background, running, total: subagents.length + background.length };
}

/** Compact duration: "45s", "23m 50s", "1h 02m". */
export function durationText(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

function cardDuration(call: ToolCall, working: boolean, now: number): string | null {
  if (call.started_at === undefined) return null;
  const from = Date.parse(call.started_at);
  if (Number.isNaN(from)) return null;
  const to = call.finished_at !== undefined ? Date.parse(call.finished_at) : working ? now : NaN;
  return Number.isNaN(to) ? null : durationText(to - from);
}

const spent = (card: AgentCard) => card.task?.total_tokens ?? card.call.tokens;

function titleOf(card: AgentCard): string {
  return card.call.detail ?? (card.kind === "subagent" ? "Subagent" : "Background task");
}

function statusLabel(card: AgentCard): string {
  if (card.task !== undefined) return card.task.status;
  if (card.kind === "background" && card.call.status !== undefined) return card.call.status;
  return card.working ? "working" : "finished";
}

function Dot({ working, failed = false }: { working: boolean; failed?: boolean }) {
  const className = working ? "agent-dot agent-dot-on" : failed ? "agent-dot agent-dot-failed" : "agent-dot";
  return <span className={className} aria-hidden="true" />;
}

/** Ended badly: a task row that says so, or a background call that does. */
function failedCard(card: AgentCard): boolean {
  const status = statusLabel(card);
  return status === "failed" || status === "killed" || status === "orphaned";
}

function Card({ card, now, onOpen }: { card: AgentCard; now: number; onOpen: () => void }) {
  const duration = cardDuration(card.call, card.working, now);
  const meta: string[] = [];
  if (card.kind === "background") meta.push(`shell${duration ? ` · ${duration}` : ""}`);
  else if (duration) meta.push(duration);
  const used = spent(card);
  if (used != null) meta.push(`${tokenCount(used)} tokens`);
  // How much it did, at a glance; what it did is one click away.
  if (card.kind === "subagent" && card.calls.length > 0)
    meta.push(plural(card.calls.length, "tool call"));
  const failed = failedCard(card);
  if (failed) meta.push(statusLabel(card));
  return (
    <button
      type="button"
      className="agent-card"
      onClick={onOpen}
      aria-label={`${titleOf(card)}: ${statusLabel(card)}`}
    >
      <span className="agent-card-title">
        <Dot working={card.working} failed={failed} />
        <span className="agent-card-text">{titleOf(card)}</span>
      </span>
      {meta.length > 0 && <span className="agent-card-meta">{meta.join(" · ")}</span>}
    </button>
  );
}

/** "working" → "Working": the status opens the facts line, so it is said as its first word. */
const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** What it is doing right now: its newest call still open, else its newest call. */
function doingNow(card: AgentCard): ToolCall | undefined {
  const open = [...card.calls].reverse().find((c) => c.finished_at === undefined);
  return open ?? card.calls[card.calls.length - 1];
}

/**
 * One agent, in a dialog of its own over the map — the map stays where it was underneath, so
 * closing this is going back to it. What it is, how long and how much, what it is doing now, what
 * it answered, and its calls; the whole run, call by call, is one more dialog away.
 */
function AgentDetail({
  card,
  now,
  chatId,
  onClose,
}: {
  card: AgentCard;
  now: number;
  /** Absent where the map is drawn outside a conversation, and Stop agent with it. */
  chatId?: string;
  onClose: () => void;
}) {
  const [transcript, setTranscript] = useState(false);
  const duration = cardDuration(card.call, card.working, now);
  const facts = [
    capital(statusLabel(card)),
    card.call.subagent_type ?? (card.kind === "background" ? "shell" : null),
    card.call.model ? displayName(card.call.model) : null,
  ].filter((x): x is string => x !== null);
  const used = spent(card);
  const timing = [duration, used != null ? `${tokenCount(used)} tokens` : null].filter(
    (x): x is string => x !== null,
  );
  const answer = card.task?.summary ?? card.call.result ?? null;
  const doing = card.working ? doingNow(card) : undefined;
  return (
    <>
      <Modal
        open
        onOpenChange={(open) => {
          if (!open) onClose();
        }}
        title={titleOf(card)}
        size="sm"
        footer={
          <>
            <Button type="button" onClick={() => setTranscript(true)}>
              Open transcript
            </Button>
            {chatId !== undefined && <StopAgent chatId={chatId} card={card} />}
          </>
        }
      >
        <div className="agent-detail">
          <p className="agent-detail-facts agent-detail-state">
            <Dot working={card.working} failed={failedCard(card)} />
            {facts.join(" · ")}
          </p>
          <p className="agent-detail-facts">
            {card.kind === "subagent" ? "Spawned by the main agent" : "A background shell task"}
          </p>
          <p className="agent-detail-facts">
            {timing.length > 0 ? timing.join(" · ") : "no timing recorded"}
          </p>
          {doing !== undefined && (
            <p className="agent-detail-doing">
              Running <span className="agent-detail-doing-what">{doing.detail ?? doing.name}</span>
            </p>
          )}
          {answer !== null && answer !== "" && (
            <details className="agent-section" open={!card.working}>
              <summary>Result</summary>
              <p className="agent-section-text" aria-label="Final answer">
                {answer}
              </p>
            </details>
          )}
          {card.kind === "subagent" && (
            <details className="agent-section">
              <summary>{`Tool calls (${card.calls.length})`}</summary>
              <ul className="agent-calls" aria-label="Its tool calls">
                {card.calls.length === 0 && (
                  <li>
                    <Quiet compact says="No tool calls recorded." />
                  </li>
                )}
                {card.calls.map((c, i) => (
                  <li key={`${c.id ?? c.name}-${i}`}>
                    <span className="agent-calls-name">{c.name}</span>
                    {c.detail !== null && <span className="agent-calls-detail">{c.detail}</span>}
                  </li>
                ))}
              </ul>
            </details>
          )}
        </div>
      </Modal>
      {transcript && <AgentTranscript card={card} onClose={() => setTranscript(false)} />}
    </>
  );
}

/**
 * Stops this one agent, and nothing else: the turn goes on, told it stopped. Offered only while it
 * works and the CLI has named its task — the name is what the stop is addressed to.
 */
function StopAgent({ chatId, card }: { chatId: string; card: AgentCard }) {
  const stop = useStopTask(chatId);
  const taskId = card.call.task_id ?? card.task?.task_id ?? undefined;
  if (!card.working) return null;
  return (
    <>
      <Button
        type="button"
        variant="danger"
        disabled={taskId === undefined || stop.isPending || stop.isSuccess}
        title={
          taskId === undefined
            ? "the CLI has not named this task yet, so it cannot be stopped on its own"
            : "stop this agent; the turn and the other agents go on"
        }
        onClick={() => {
          if (taskId !== undefined) stop.mutate(taskId);
        }}
      >
        {stop.isSuccess ? "Stopping…" : "Stop agent"}
      </Button>
      {stop.isError && (
        <p className="agent-stop-error" role="status">
          the turn is no longer running, so nothing was there to stop it
        </p>
      )}
    </>
  );
}

/**
 * The whole run of one agent, call by call, the way Claude Code shows a subagent: each call named,
 * what went IN and what came OUT. The results are not on the transcript poll (see `ToolCall.result`),
 * so they are read here, once, from the turn's own tool record.
 */
function AgentTranscript({ card, onClose }: { card: AgentCard; onClose: () => void }) {
  const tools = useTurnTools(card.turnId ?? 0, card.turnId !== undefined);
  const full = new Map<string, ToolCall>();
  for (const c of tools.data?.did ?? []) if (c.id !== undefined) full.set(c.id, c);
  const steps = card.kind === "subagent" ? card.calls : [card.call];
  const own = card.call.id !== undefined ? full.get(card.call.id) : undefined;
  const answer = card.task?.summary ?? own?.result ?? card.call.result ?? null;
  return (
    <Modal
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      title={titleOf(card)}
      size="lg"
    >
      <ol className="agent-transcript" aria-label="Transcript">
        {steps.length === 0 && (
          <li>
            <Quiet compact says="No tool calls recorded." />
          </li>
        )}
        {steps.map((step, i) => {
          const c = (step.id !== undefined ? full.get(step.id) : undefined) ?? step;
          const running = c.finished_at === undefined && card.working;
          const failed = c.result_failed === true || c.status === "failed";
          const dot = running
            ? "agent-step-dot agent-step-dot-on"
            : failed
              ? "agent-step-dot agent-step-dot-failed"
              : "agent-step-dot";
          const out =
            c.result != null && c.result !== ""
              ? c.result
              : running
                ? "running…"
                : tools.isPending && card.turnId !== undefined
                  ? "reading…"
                  : "no output recorded";
          return (
            <li key={`${c.id ?? c.name}-${i}`} className="agent-step">
              <span className={dot} aria-hidden="true" />
              <p className="agent-step-name">{c.name}</p>
              <div className="agent-io">
                {c.detail !== null && (
                  <div className="agent-io-row">
                    <span className="agent-io-tag">IN</span>
                    <pre className="agent-io-text">{c.detail}</pre>
                  </div>
                )}
                <div className="agent-io-row">
                  <span className="agent-io-tag">OUT</span>
                  <pre className={failed ? "agent-io-text agent-io-failed" : "agent-io-text"}>{out}</pre>
                </div>
              </div>
            </li>
          );
        })}
        {answer !== null && answer !== "" && (
          <li className="agent-step">
            <span className="agent-step-dot" aria-hidden="true" />
            <p className="agent-step-name">Answer</p>
            <p className="agent-step-answer">{answer}</p>
          </li>
        )}
        {card.working && (
          <li className="agent-step">
            <span className="agent-step-dot agent-step-dot-on" aria-hidden="true" />
            <p className="agent-step-name agent-step-live">Working…</p>
          </li>
        )}
      </ol>
    </Modal>
  );
}

/** The root card, its subagents to the right on a connector, and the background tasks below. */
export function AgentMapBody({
  turns,
  tasks,
  chatTitle,
  now,
  chatId,
}: {
  turns: Turn[];
  tasks?: ChatTask[];
  chatTitle: string;
  now: number;
  /** The conversation, for Stop agent; absent, the detail offers no stop. */
  chatId?: string;
}) {
  const groups = useMemo(() => groupAgents(turns, tasks), [turns, tasks]);
  const [picked, setPicked] = useState<string | null>(null);
  const all = [...groups.subagents, ...groups.background];
  const card = picked === null ? undefined : all.find((c) => c.key === picked);

  const last = turns[turns.length - 1];
  // The orchestrator's tokens, said the way its subagents' are: what its context holds, from the
  // newest turn that reported it — a running turn has not yet, and the one before it still holds.
  const filled = [...turns].reverse().find((t) => t.contextFill != null)?.contextFill ?? null;
  const rootMeta = [
    last?.model ? displayName(last.model) : null,
    filled !== null ? `${tokenCount(filled)} tokens` : "tokens not reported yet",
  ].filter((x): x is string => x !== null);
  return (
    <div className="agent-map">
      {card !== undefined && (
        <AgentDetail card={card} now={now} chatId={chatId} onClose={() => setPicked(null)} />
      )}
      <div className="agent-tree">
        <div className="agent-root">
          <span className="agent-card agent-card-root">
            <span className="agent-card-title">
              <Dot working={groups.running > 0} />
              <span className="agent-card-text">{chatTitle}</span>
            </span>
            {rootMeta.length > 0 && <span className="agent-card-meta">{rootMeta.join(" · ")}</span>}
          </span>
        </div>
        {groups.subagents.length > 0 && (
          <ul className="agent-children" aria-label="Subagents">
            {groups.subagents.map((c) => (
              <li key={c.key} className="agent-child">
                <Card card={c} now={now} onOpen={() => setPicked(c.key)} />
              </li>
            ))}
          </ul>
        )}
      </div>
      {groups.background.length > 0 && (
        <section className="agent-bg">
          <h3 className="agent-bg-title">
            {plural(groups.background.length, "background task")}
          </h3>
          <div className="agent-bg-cards">
            {groups.background.map((c) => (
              <Card key={c.key} card={c} now={now} onOpen={() => setPicked(c.key)} />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

/**
 * The two chips in a conversation's header: cache time left, and the agent count that opens the map.
 *
 * Every field it reads is optional on the wire; a daemon that sends none gets the cache chip on its
 * 5m assumption (marked "~") and no agent chip at all.
 */
export function ChatChips({
  chatId,
  turns,
  chatTitle,
}: {
  chatId: string;
  turns: Turn[] | undefined;
  chatTitle: string;
}) {
  const [open, setOpen] = useState(false);
  const now = useNow(open ? 1000 : 30_000);
  const last = turns?.[turns.length - 1];
  const live = last !== undefined && turnIsLive(last.status);
  // A live turn's newest calls reach the live poll before the transcript; merge them in so a
  // subagent shows up while it is working.
  // Read from the cache the page's own live poll fills; polling here too doubled the requests.
  const liveSnapshot = useLiveTurnSnapshot(last?.id ?? 0);
  const merged = useMemo(() => {
    if (turns === undefined) return undefined;
    const liveDid = live ? liveSnapshot?.did : undefined;
    if (!live || liveDid === undefined || liveDid.length === 0) return turns;
    return turns.map((t) => (t.id === last?.id && liveDid.length >= t.did.length ? { ...t, did: liveDid } : t));
  }, [turns, liveSnapshot, live, last?.id]);

  const cache = cacheState(merged, now);
  const base = useMemo(() => groupAgents(merged), [merged]);
  const tasks = useChatTasks(chatId, base.total > 0, live);
  const groups = useMemo(() => groupAgents(merged, tasks.data), [merged, tasks.data]);
  if (merged === undefined || (cache === null && groups.total === 0)) return null;

  const cacheTitle =
    "Prompt cache: while it is warm, the next message re-reads the conversation at a fraction of the cost. " +
    "It runs from the end of the last turn, and every turn refreshes it." +
    (cache?.kind === "refreshing" ? " A turn is running now, so it is being refreshed." : "") +
    (cache?.approx ? " The daemon did not say which lifetime applies, so 5 minutes is assumed." : "");
  const shown = groups.running > 0 ? groups.running : groups.total;
  return (
    <>
      {cache !== null && (
        <span className={cache.kind === "cold" ? "chats-chip chats-chip-muted" : "chats-chip"} title={cacheTitle}>
          <span aria-hidden="true">⏱</span>{" "}
          {/* Only the time: the rest of the sentence — refreshing, assumed lifetime — is on the title. */}
          {cache.kind === "cold" ? "cold" : leftText(cache.ms)}
        </span>
      )}
      {groups.total > 0 && (
        <button
          type="button"
          className={groups.running > 0 ? "chats-chip chats-chip-btn" : "chats-chip chats-chip-btn chats-chip-muted"}
          onClick={() => setOpen(true)}
          title="Open the agent map"
        >
          <span aria-hidden="true">●</span> {plural(shown, "agent")}
        </button>
      )}
      {open && (
        <Modal open onOpenChange={setOpen} title="Agent map" size="md">
          <AgentMapBody turns={merged} tasks={tasks.data} chatTitle={chatTitle} now={now} chatId={chatId} />
        </Modal>
      )}
    </>
  );
}
