import type { Command, CommandCard, CommandSource } from "../data/chats";
import type { McpCardData, McpImportReport } from "../data/mcp";
import { McpCardBody, McpImportBody } from "./McpPanel";

/** Where a command came from, in the words the picker and the help card both use. */
export function sourceLabel(source: CommandSource, kind?: Command["kind"]): string {
  const where =
    source === "builtin"
      ? "built-in"
      : source === "project"
        ? "project"
        : source === "personal"
          ? "personal"
          : "plugin";
  return kind === "skill" ? `skill · ${where}` : where;
}

interface HelpEntry {
  name: string;
  kind?: "command" | "skill";
  hint?: string | null;
  description?: string | null;
  aliases?: string[];
  partial?: string[];
  shadowed_by?: string | null;
}

interface HelpGroup {
  source: CommandSource;
  entries: HelpEntry[];
}

function EntryLine({ entry, source }: { entry: HelpEntry; source: CommandSource }) {
  return (
    <li className="chats-slash-entry">
      <span className="chats-mention-name">
        /{entry.name}
        {entry.hint ? ` ${entry.hint}` : ""}
      </span>{" "}
      <span className="chats-mention-path">
        {entry.description ?? ""}
        {entry.aliases && entry.aliases.length > 0
          ? ` (also ${entry.aliases.map((a) => `/${a}`).join(", ")})`
          : ""}
        {entry.shadowed_by ? ` · shadowed by ${entry.shadowed_by}` : ""}
      </span>{" "}
      <span className="chats-slash-source">{sourceLabel(source, entry.kind)}</span>
    </li>
  );
}

/** Money as the cards write it: dollars and cents. */
function usd(value: number): string {
  return `$${value.toFixed(2)}`;
}

/** A token count with thousands separators, or nothing when the engine did not measure it. */
function tokens(value: number | null | undefined): string | null {
  return typeof value === "number" ? value.toLocaleString("en-US") : null;
}

const NO_MEASURE = "No measure on this engine";

interface StateData {
  text?: string;
  stopped?: "interrupted" | "killed";
  // context
  engine?: string;
  model?: string | null;
  window_tokens?: number | null;
  used_tokens?: number | null;
  peak_tokens?: number | null;
  percent?: number | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_read_tokens?: number | null;
  cache_creation_tokens?: number | null;
  live_session?: boolean;
  compacted?: boolean;
  /** The `/compact` in force: the next turns start from its handoff. Null when there is none. */
  compaction?: { run_id: number; at: string } | null;
  turns?: number;
  note?: string | null;
  // cost
  total_usd?: number;
  unmeasured_turns?: number;
  since_clear_usd?: number;
  last_turn_usd?: number | null;
  turn_budget_usd?: number | null;
  // effort
  current?: string | null;
  efforts?: string[];
  // dirs
  cwd?: string | null;
  extra_dirs?: string[];
  // budget
  warning?: string | null;
  global?: BudgetSnapshot | null;
}

/** The house budget as the core's `BudgetSnapshot` serializes it (snake_case, no renames). */
interface BudgetSnapshot {
  limit_usd: number | null;
  period: string;
  hourly_limit_usd?: number | null;
  window_spend_usd: number;
  hourly_spend_usd?: number;
  paused: boolean;
  reason?: string | null;
}

function ContextBody({ data }: { data: StateData }) {
  const used = tokens(data.used_tokens);
  const window = tokens(data.window_tokens);
  const detail: string[] = [];
  const peak = tokens(data.peak_tokens);
  if (peak !== null) detail.push(`peak ${peak}`);
  const input = tokens(data.input_tokens);
  if (input !== null) detail.push(`${input} in`);
  const output = tokens(data.output_tokens);
  if (output !== null) detail.push(`${output} out`);
  const cacheRead = tokens(data.cache_read_tokens);
  if (cacheRead !== null) detail.push(`${cacheRead} cache read`);
  const cacheWrite = tokens(data.cache_creation_tokens);
  if (cacheWrite !== null) detail.push(`${cacheWrite} cache written`);
  return (
    <>
      <p>
        {[data.engine, data.model].filter((part) => part != null && part !== "").join(" · ")}
      </p>
      {used === null ? (
        <p>{data.note ?? NO_MEASURE}</p>
      ) : (
        <p>
          {used}
          {window !== null ? ` of ${window}` : ""} tokens
          {typeof data.percent === "number" ? ` (${Math.round(data.percent)}%)` : ""}
        </p>
      )}
      {used !== null && data.note != null && <p>{data.note}</p>}
      {used !== null && detail.length > 0 && <p>{detail.join(" · ")}</p>}
      {typeof data.turns === "number" && <p>{data.turns === 1 ? "1 turn" : `${data.turns} turns`}</p>}
      {data.compacted === true && <p>Compacted</p>}
      {/* Raw: no other date on this card is formatted, and the daemon's stamp is already legible. */}
      {data.compaction != null && <p>Compaction in force since {data.compaction.at}</p>}
    </>
  );
}

function CostBody({ data }: { data: StateData }) {
  const total = data.total_usd ?? 0;
  const turns = data.turns ?? 0;
  const unmeasured = data.unmeasured_turns ?? 0;
  const since = data.since_clear_usd;
  return (
    <>
      <p>
        Total {usd(total)} over {turns === 1 ? "1 turn" : `${turns} turns`}
      </p>
      {unmeasured > 0 && (
        <p>{unmeasured === 1 ? "1 turn with no cost" : `${unmeasured} turns with no cost`}</p>
      )}
      {typeof since === "number" && since !== total && <p>Since clear {usd(since)}</p>}
      {typeof data.last_turn_usd === "number" && <p>Last turn {usd(data.last_turn_usd)}</p>}
      {typeof data.turn_budget_usd === "number" && (
        <p>Per-turn budget {usd(data.turn_budget_usd)}</p>
      )}
    </>
  );
}

/**
 * What a slash command answered with, drawn beside the transcript. Held in memory only: a card is
 * an answer to the person at the keyboard, not part of the conversation, so it is never replayed.
 */
export function SlashCard({
  card,
  chatId,
  onDismiss,
}: {
  card: CommandCard;
  /** The conversation the card answers for; the `mcp` card switches servers in it. */
  chatId?: string;
  onDismiss: () => void;
}) {
  const data = (card.data ?? {}) as {
    groups?: HelpGroup[];
    entry?: HelpEntry;
    source?: CommandSource;
  } & StateData;
  return (
    <section className="chats-slash-card" aria-label={card.kind === "help" ? "Commands" : "Command"}>
      {card.kind === "help" &&
        (data.groups ?? []).map((group) => (
          <div key={group.source}>
            <h4 className="chats-slash-group">{sourceLabel(group.source)}</h4>
            <ul>
              {(group.entries ?? []).map((entry) => (
                <EntryLine key={entry.name} entry={entry} source={group.source} />
              ))}
            </ul>
          </div>
        ))}
      {card.kind === "help_entry" && data.entry !== undefined && (
        <ul>
          <EntryLine entry={data.entry} source={data.source ?? "builtin"} />
        </ul>
      )}
      {card.kind === "notice" && <p>{data.text ?? ""}</p>}
      {card.kind === "context" && <ContextBody data={data} />}
      {card.kind === "cost" && <CostBody data={data} />}
      {card.kind === "effort" && (
        <>
          <p>Effort is {data.current ?? "not set"}</p>
          {(data.efforts ?? []).length > 0 && <p>Levels: {(data.efforts ?? []).join(", ")}</p>}
        </>
      )}
      {card.kind === "dirs" && (
        <>
          <p>Working directory: {data.cwd ?? "none"}</p>
          {(data.extra_dirs ?? []).length > 0 ? (
            <ul>
              {(data.extra_dirs ?? []).map((dir) => (
                <li key={dir}>{dir}</li>
              ))}
            </ul>
          ) : (
            <p>No extra directories</p>
          )}
        </>
      )}
      {card.kind === "budget" && (
        <>
          <p>
            {typeof data.turn_budget_usd === "number"
              ? `Per-turn budget ${usd(data.turn_budget_usd)}`
              : "No per-turn budget"}
          </p>
          {data.global != null && typeof data.global.limit_usd === "number" && (
            <p>
              Global budget {usd(data.global.limit_usd)} per {data.global.period},{" "}
              {usd(data.global.window_spend_usd)} spent
            </p>
          )}
          {data.global != null && data.global.paused && (
            <p>
              Paused
              {data.global.reason != null && data.global.reason !== ""
                ? `: ${data.global.reason}`
                : ""}
            </p>
          )}
          {data.warning != null && data.warning !== "" && <p>{data.warning}</p>}
        </>
      )}
      {card.kind === "mcp" && chatId !== undefined && card.data != null && (
        <McpCardBody chatId={chatId} data={card.data as McpCardData} />
      )}
      {card.kind === "mcp_import" && card.data != null && (
        <McpImportBody data={card.data as McpImportReport} />
      )}
      <button type="button" className="chats-slash-dismiss" onClick={onDismiss}>
        Dismiss
      </button>
    </section>
  );
}
