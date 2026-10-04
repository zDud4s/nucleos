import { useEffect, useMemo, useState } from "react";
import { ArrowLeft } from "lucide-react";
import { useLiveTurn } from "../data/chats";
import { turnIsLive, type ToolCall, type Turn } from "../lib/turns";
import { displayName } from "../lib/modelName";
import { Modal } from "../ui/Modal";
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
  /** Calls this subagent made itself: those whose `parent` is its id. */
  calls: ToolCall[];
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
 * no `finished_at`); a subagent with no `finished_at` on a live turn. A settled turn has no working
 * subagent, whatever the fields say, because nothing the page can see outlives its turn.
 */
export function groupAgents(turns: Turn[] | undefined): AgentGroups {
  const subagents: AgentCard[] = [];
  const background: AgentCard[] = [];
  for (const turn of turns ?? []) {
    const live = turnIsLive(turn.status);
    turn.did.forEach((call, index) => {
      const key = call.id ?? `${turn.id}-${index}`;
      if (isSubagent(call)) {
        const calls = turn.did.filter(
          (c) => c.parent !== undefined && c.parent !== null && c.parent === call.id,
        );
        subagents.push({ key, kind: "subagent", call, working: live && !call.finished_at, calls });
      } else if (call.background === true) {
        // Only a live turn's word is current: a settled turn's row was saved when the turn ended,
        // and a task still running then stays "running" in it forever.
        const running =
          live && (call.status !== undefined ? call.status === "running" : !call.finished_at);
        background.push({ key, kind: "background", call, working: running, calls: [] });
      }
    });
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

const tokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));

function titleOf(card: AgentCard): string {
  return card.call.detail ?? (card.kind === "subagent" ? "Subagent" : "Background task");
}

function statusLabel(card: AgentCard): string {
  if (card.kind === "background" && card.call.status !== undefined) return card.call.status;
  return card.working ? "working" : "finished";
}

function Dot({ working }: { working: boolean }) {
  return <span className={working ? "agent-dot agent-dot-on" : "agent-dot"} aria-hidden="true" />;
}

function Card({ card, now, onOpen }: { card: AgentCard; now: number; onOpen: () => void }) {
  const duration = cardDuration(card.call, card.working, now);
  const meta: string[] = [];
  if (card.kind === "background") meta.push(`shell${duration ? ` · ${duration}` : ""}`);
  else if (duration) meta.push(duration);
  if (card.call.tokens !== undefined) meta.push(`${tokens(card.call.tokens)} tokens`);
  return (
    <button
      type="button"
      className="agent-card"
      onClick={onOpen}
      aria-label={`${titleOf(card)} — ${statusLabel(card)}`}
    >
      <span className="agent-card-title">
        <Dot working={card.working} />
        <span className="agent-card-text">{titleOf(card)}</span>
      </span>
      {meta.length > 0 && <span className="agent-card-meta">{meta.join(" · ")}</span>}
    </button>
  );
}

function Detail({ card, now, onBack }: { card: AgentCard; now: number; onBack: () => void }) {
  const duration = cardDuration(card.call, card.working, now);
  const facts = [
    statusLabel(card),
    card.call.subagent_type ?? (card.kind === "background" ? "shell" : null),
    card.call.model ? displayName(card.call.model) : null,
  ].filter((x): x is string => x !== null);
  const spent = [duration, card.call.tokens !== undefined ? `${tokens(card.call.tokens)} tokens` : null].filter(
    (x): x is string => x !== null,
  );
  return (
    <div className="agent-detail">
      <button type="button" className="agent-back" onClick={onBack} aria-label="Back to the map">
        <ArrowLeft aria-hidden="true" />
      </button>
      <h3 className="agent-detail-title">{titleOf(card)}</h3>
      <p className="agent-detail-facts">{facts.join(" · ")}</p>
      <p className="agent-detail-facts">{spent.length > 0 ? spent.join(" · ") : "no timing recorded"}</p>
      {card.kind === "subagent" && (
        <ul className="agent-calls" aria-label="Its tool calls">
          {card.calls.length === 0 && <li className="agent-calls-none">No tool calls recorded.</li>}
          {card.calls.map((c, i) => (
            <li key={`${c.id ?? c.name}-${i}`}>
              <span className="agent-calls-name">{c.name}</span>
              {c.detail !== null && <span className="agent-calls-detail">{c.detail}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The root card, its subagents to the right on a connector, and the background tasks below. */
export function AgentMapBody({
  turns,
  chatTitle,
  now,
}: {
  turns: Turn[];
  chatTitle: string;
  now: number;
}) {
  const groups = useMemo(() => groupAgents(turns), [turns]);
  const [picked, setPicked] = useState<string | null>(null);
  const all = [...groups.subagents, ...groups.background];
  const card = picked === null ? undefined : all.find((c) => c.key === picked);
  if (card !== undefined) return <Detail card={card} now={now} onBack={() => setPicked(null)} />;

  const last = turns[turns.length - 1];
  const rootMeta = [
    last?.model ? displayName(last.model) : null,
    last?.contextFill != null ? `${tokens(last.contextFill)} context` : null,
  ].filter((x): x is string => x !== null);
  return (
    <div className="agent-map">
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
            {groups.background.length} background task{groups.background.length === 1 ? "" : "s"}
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

/** Re-render on a timer. Local to the chips: the rest of the page does not need the tick. */
function useNow(everyMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(id);
  }, [everyMs]);
  return now;
}

/**
 * The two chips in a conversation's header: cache time left, and the agent count that opens the map.
 *
 * Every field it reads is optional on the wire; a daemon that sends none gets the cache chip on its
 * 5m assumption (marked "~") and no agent chip at all.
 */
export function ChatChips({ turns, chatTitle }: { turns: Turn[] | undefined; chatTitle: string }) {
  const [open, setOpen] = useState(false);
  const now = useNow(open ? 1000 : 30_000);
  const last = turns?.[turns.length - 1];
  const live = last !== undefined && turnIsLive(last.status);
  // A live turn's newest calls reach the live poll before the transcript; merge them in so a
  // subagent shows up while it is working.
  const liveTurn = useLiveTurn(last?.id ?? 0, live);
  const merged = useMemo(() => {
    if (turns === undefined) return undefined;
    const liveDid = liveTurn.data?.did;
    if (!live || liveDid === undefined || liveDid.length === 0) return turns;
    return turns.map((t) => (t.id === last?.id && liveDid.length >= t.did.length ? { ...t, did: liveDid } : t));
  }, [turns, liveTurn.data, live, last?.id]);

  const cache = cacheState(merged, now);
  const groups = useMemo(() => groupAgents(merged), [merged]);
  if (merged === undefined || (cache === null && groups.total === 0)) return null;

  const cacheTitle =
    "Prompt cache: while it is warm, the next message re-reads the conversation at a fraction of the cost. " +
    "It runs from the end of the last turn, and every turn refreshes it." +
    (cache?.approx ? " The daemon did not say which lifetime applies, so 5 minutes is assumed." : "");
  const shown = groups.running > 0 ? groups.running : groups.total;
  return (
    <>
      {cache !== null && (
        <span className={cache.kind === "cold" ? "chats-chip chats-chip-muted" : "chats-chip"} title={cacheTitle}>
          <span aria-hidden="true">⏱</span>{" "}
          {cache.kind === "cold" ? "cold" : `${cache.approx ? "~" : ""}${leftText(cache.ms)}`}
          {cache.kind === "refreshing" && <span className="chats-chip-note"> refreshing</span>}
        </span>
      )}
      {groups.total > 0 && (
        <button
          type="button"
          className={groups.running > 0 ? "chats-chip chats-chip-btn" : "chats-chip chats-chip-btn chats-chip-muted"}
          onClick={() => setOpen(true)}
          title="Open the agent map"
        >
          <span aria-hidden="true">●</span> {shown} agent{shown === 1 ? "" : "s"}
        </button>
      )}
      {open && (
        <Modal open onOpenChange={setOpen} title="Agent map" size="md">
          <AgentMapBody turns={merged} chatTitle={chatTitle} now={now} />
        </Modal>
      )}
    </>
  );
}
