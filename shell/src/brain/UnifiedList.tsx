import { DAEMON_NAME } from "../copy";
import { useState, type Dispatch, type SetStateAction } from "react";
import { Check, Search, X } from "lucide-react";
import { useAllCaptures } from "../data/captures";
import { useApproveKnowledge, useKnowledge, useRejectKnowledge, type Known, type KnownLayer } from "../data/knowledge";
import { useOwnerNotes, useSearchOwnerNotes } from "../data/owner-notes";
import { Button, ErrorNote, Quiet, RelativeTime, Row, Rows, Teach } from "../ui";
import { Segments } from "./Segments";
import { formatItem } from "./item-ref";
import { MeasuredSummary } from "./knowledge/MeasuredSummary";
import { filterItems, itemDate, itemId, scopeKey, toItems, type BrainItem, type ItemFilters } from "./unified";
import "./unified-list.css";

export interface UnifiedListProps {
  /** Receives `formatItem(ref)`: `note:12`, `knowledge:5` or `capture:7`. */
  onSelect(item: string): void;
  /** The item open in the side panel, so the list does not draw its form twice. */
  selected?: string;
}

const TYPE_FILTERS: readonly (readonly [ItemFilters["type"], string])[] = [
  ["all", "All"],
  ["note", "Notes"],
  ["knowledge", "Knowledge"],
  ["capture", "Captures"],
];

const STATE_FILTERS: readonly (readonly [ItemFilters["state"], string])[] = [
  ["in_force", "In force"],
  ["out", "Over"],
  ["all", "Any state"],
];

const LAYER_FILTERS: readonly (readonly [KnownLayer | "all", string])[] = [
  ["all", "All layers"],
  ["semantic", "Facts"],
  ["episodic", "Measured"],
  ["procedural", "How-to"],
  ["working", "Working"],
];

/**
 * State that outlives the component: List and Graph unmount each other, and a filter lost on the
 * way back is a filter the owner sets twice. Session storage keeps it for the tab (never across
 * sessions); where storage is blocked it is plain state.
 */
function useKept<T extends string>(key: string, initial: T, valid: (value: string) => boolean): [T, Dispatch<SetStateAction<T>>] {
  const [value, setValue] = useState<T>(() => {
    try {
      const kept = window.sessionStorage.getItem(`brain.list.${key}`);
      return kept !== null && valid(kept) ? (kept as T) : initial;
    } catch {
      return initial;
    }
  });
  const set: Dispatch<SetStateAction<T>> = (next) => {
    setValue((prev) => {
      const resolved = typeof next === "function" ? (next as (p: T) => T)(prev) : next;
      try {
        window.sessionStorage.setItem(`brain.list.${key}`, resolved);
      } catch {
        // storage blocked: the filter still works for this mount
      }
      return resolved;
    });
  };
  return [value, set];
}

/** The attention-ordered list: what waits for a decision, then notes and knowledge by date. */
export function UnifiedList({ onSelect, selected }: UnifiedListProps) {
  const [type, setType] = useKept<ItemFilters["type"]>("type", "all", (v) => TYPE_FILTERS.some(([k]) => k === v));
  const [state, setState] = useKept<ItemFilters["state"]>("state", "in_force", (v) =>
    STATE_FILTERS.some(([k]) => k === v),
  );
  const [layer, setLayer] = useKept<KnownLayer | "all">("layer", "all", (v) => LAYER_FILTERS.some(([k]) => k === v));
  const [scope, setScope] = useKept<string>("scope", "all", () => true);
  const [q, setQ] = useKept<string>("q", "", () => true);

  const searching = q.trim() !== "";
  const allNotes = useOwnerNotes("all");
  const found = useSearchOwnerNotes(q);
  const knowledge = useKnowledge();
  const captureRows = useAllCaptures();
  const approve = useApproveKnowledge();
  const reject = useRejectKnowledge();

  const noteRows = (searching ? found.data : allNotes.data) ?? [];
  const known = knowledge.data ?? [];
  const items = filterItems(toItems(noteRows, known, captureRows.data ?? []), { type, state, layer, scope, q });
  // Only the row in flight is held; the others stay usable while one decision is out.
  const inFlight = approve.isPending ? approve.variables : reject.isPending ? reject.variables : undefined;
  const decidingId = typeof inFlight === "object" ? inFlight.proposalId : inFlight;
  const decided = approve.isSuccess ? "Lesson approved" : reject.isSuccess ? "Lesson refused" : "";
  const scopes = scopeOptions(known);

  const nothingAtAll =
    allNotes.data !== undefined &&
    knowledge.data !== undefined &&
    captureRows.data !== undefined &&
    allNotes.data.length === 0 &&
    knowledge.data.length === 0 &&
    captureRows.data.length === 0;

  if (nothingAtAll) {
    return (
      <Teach title="Nothing has been learned yet">
        This is where your notes, supplemental instructions, facts about a project, and reusable
        ways of working are kept once you have approved them. Nothing reaches a prompt until you
        say so, so an empty layer means the agent is running on its standing brief alone.
      </Teach>
    );
  }

  return (
    <>
      {(allNotes.isError || knowledge.isError || captureRows.isError) && (
        <ErrorNote>the {DAEMON_NAME} is not responding — part of what is known is missing</ErrorNote>
      )}
      <div className="unified-bar">
        <Segments label="Type" value={type} options={TYPE_FILTERS} onChange={setType} />
        <Segments label="State" value={state} options={STATE_FILTERS} onChange={setState} />
        <label className="unified-search">
          <Search aria-hidden="true" size={14} strokeWidth={1.75} />
          <input
            type="search"
            aria-label="Search"
            placeholder="Search notes, knowledge and questions"
            value={q}
            onChange={(event) => setQ(event.target.value)}
          />
        </label>
        {type === "knowledge" && (
          <div className="unified-bar-knowledge">
            <Segments label="Layer" value={layer} options={LAYER_FILTERS} onChange={setLayer} />
            <select
              className="unified-scope"
              aria-label="Scope"
              value={scope}
              onChange={(event) => setScope(event.target.value)}
            >
              <option value="all">All scopes</option>
              {scopes.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>
        )}
      </div>

      <span className="sr-only" role="status">
        {decided}
      </span>
      {items.length === 0 ? (
        <Quiet says={searching ? "Nothing matches that." : "Nothing here for these filters."} />
      ) : (
        <Rows label="Notes and knowledge" className="unified-rows">
          {items.map((item) => (
            <ItemRow
              key={`${item.kind}:${itemId(item)}`}
              item={item}
              current={selected === itemRef(item)}
              onSelect={onSelect}
              deciding={decidingId}
              onApprove={(id) => approve.mutate(id)}
              onReject={(id) => reject.mutate(id)}
            />
          ))}
        </Rows>
      )}

      <details className="unified-summary">
        <summary>Measured, by generator</summary>
        <MeasuredSummary rows={known} />
      </details>
    </>
  );
}

interface ItemRowProps {
  item: BrainItem;
  current: boolean;
  onSelect(item: string): void;
  /** Proposal id whose decision is in flight, if any. */
  deciding: number | undefined;
  onApprove(proposalId: number): void;
  onReject(proposalId: number): void;
}

function itemRef(item: BrainItem): string {
  return item.kind === "note"
    ? formatItem({ kind: "note", id: item.note.id })
    : item.kind === "capture"
      ? formatItem({ kind: "capture", id: item.capture.job_id })
      : formatItem({ kind: "knowledge", id: item.known.id });
}

/**
 * What a row says it is, and which identity hue its mark takes — the same hue its node takes on
 * the graph (`force-graph.css`), so the list and the map share one key. A knowledge row names its
 * layer, which is what decides how it is used; "knowledge" alone would say nothing a reader can act on.
 */
const LAYER_WORD: Record<KnownLayer, string> = {
  semantic: "fact",
  episodic: "measured",
  procedural: "how-to",
  working: "working",
};

function kindOf(item: BrainItem): { word: string; hue: string } {
  if (item.kind === "note") return { word: "note", hue: "note" };
  if (item.kind === "capture") return { word: "asked", hue: "capture" };
  return { word: LAYER_WORD[item.known.layer], hue: `k-${item.known.layer}` };
}

function ItemRow({ item, current, onSelect, deciding, onApprove, onReject }: ItemRowProps) {
  const ref = itemRef(item);
  const kind = kindOf(item);
  const title =
    item.kind === "note"
      ? firstLine(item.note.text)
      : item.kind === "capture"
        ? firstLine(item.capture.prompt_text)
        : item.known.title;
  const proposalId =
    item.kind === "knowledge" && item.known.status === "proposed" ? item.known.proposal_id : null;
  return (
    <Row layout="line" current={current} className="unified-row">
      <button type="button" className="unified-open" aria-current={current ? "true" : undefined} onClick={() => onSelect(ref)}>
        <span className="unified-kind">
          <span className={`unified-mark unified-mark-${kind.hue}`} aria-hidden="true" />
          {kind.word}
        </span>
        <span className="unified-title">{title}</span>
        <span className="unified-when">
          <RelativeTime at={itemDate(item)} />
        </span>
      </button>
      {proposalId !== null && (
        <span className="unified-decide">
          <Button
            variant="approve"
            aria-label={`Approve ${title}`}
            title="Approve: the agent will use this text in its prompts"
            disabled={deciding === proposalId}
            onClick={() => onApprove(proposalId)}
          >
            <Check aria-hidden="true" size={14} strokeWidth={2} />
          </Button>
          <Button
            variant="ghost"
            aria-label={`Refuse ${title}`}
            title="Refuse: the agent will not use this"
            disabled={deciding === proposalId}
            onClick={() => onReject(proposalId)}
          >
            <X aria-hidden="true" size={14} strokeWidth={2} />
          </Button>
        </span>
      )}
    </Row>
  );
}

function firstLine(text: string): string {
  const line = text.split("\n").find((l) => l.trim() !== "");
  return (line ?? text).trim();
}

function scopeOptions(rows: Known[]) {
  const options = new Map<string, { value: string; label: string }>();
  for (const row of rows) {
    options.set(scopeKey(row), { value: scopeKey(row), label: row.scope_id ?? "this machine" });
  }
  return [...options.values()].sort((a, b) => a.label.localeCompare(b.label));
}
