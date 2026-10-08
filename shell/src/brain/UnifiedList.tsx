import { useState } from "react";
import { useApproveKnowledge, useKnowledge, useRejectKnowledge, type Known, type KnownLayer } from "../data/knowledge";
import { useOwnerNotes, useSearchOwnerNotes } from "../data/owner-notes";
import { Button, ErrorNote, Panel, Quiet, RelativeTime, Row, Rows, Teach } from "../ui";
import { formatItem } from "./item-ref";
import { MeasuredSummary } from "./knowledge/MeasuredSummary";
import { WaitingPanel } from "./knowledge/WaitingPanel";
import { filterItems, itemDate, scopeKey, toItems, type BrainItem, type ItemFilters } from "./unified";
import "./unified-list.css";

export interface UnifiedListProps {
  /** Receives `formatItem(ref)`: `note:12` or `knowledge:5`. */
  onSelect(item: string): void;
}

const TYPE_FILTERS: readonly (readonly [ItemFilters["type"], string])[] = [
  ["all", "All"],
  ["note", "Notes"],
  ["knowledge", "Knowledge"],
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

/** The attention-ordered list: what waits for a decision, then notes and knowledge by date. */
export function UnifiedList({ onSelect }: UnifiedListProps) {
  const [type, setType] = useState<ItemFilters["type"]>("all");
  const [state, setState] = useState<ItemFilters["state"]>("in_force");
  const [layer, setLayer] = useState<KnownLayer | "all">("all");
  const [scope, setScope] = useState("all");
  const [q, setQ] = useState("");

  const searching = q.trim() !== "";
  const allNotes = useOwnerNotes("all");
  const found = useSearchOwnerNotes(q);
  const knowledge = useKnowledge();
  const approve = useApproveKnowledge();
  const reject = useRejectKnowledge();

  const noteRows = (searching ? found.data : allNotes.data) ?? [];
  const known = knowledge.data ?? [];
  const items = filterItems(toItems(noteRows, known), { type, state, layer, scope, q });
  const waiting = known.filter((row) => row.status === "proposed");
  const deciding = approve.isPending || reject.isPending;
  const scopes = scopeOptions(known);

  const nothingAtAll =
    allNotes.data !== undefined &&
    knowledge.data !== undefined &&
    allNotes.data.length === 0 &&
    knowledge.data.length === 0;

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
      {(allNotes.isError || knowledge.isError) && (
        <ErrorNote>the núcleo did not answer — part of what is known is missing</ErrorNote>
      )}
      <WaitingPanel rows={waiting} />

      <div className="unified-filters">
        <div role="group" aria-label="Type">
          {TYPE_FILTERS.map(([value, label]) => (
            <Button key={value} variant="quiet" aria-pressed={type === value} onClick={() => setType(value)}>
              {label}
            </Button>
          ))}
        </div>
        <div role="group" aria-label="State">
          {STATE_FILTERS.map(([value, label]) => (
            <Button key={value} variant="quiet" aria-pressed={state === value} onClick={() => setState(value)}>
              {label}
            </Button>
          ))}
        </div>
        {type !== "note" && (
          <>
            <div role="group" aria-label="Layer">
              {LAYER_FILTERS.map(([value, label]) => (
                <Button key={value} variant="quiet" aria-pressed={layer === value} onClick={() => setLayer(value)}>
                  {label}
                </Button>
              ))}
            </div>
            <label>
              Scope{" "}
              <select aria-label="Scope" value={scope} onChange={(event) => setScope(event.target.value)}>
                <option value="all">All scopes</option>
                {scopes.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
          </>
        )}
        <label>
          Search{" "}
          <input type="search" aria-label="Search" value={q} onChange={(event) => setQ(event.target.value)} />
        </label>
      </div>

      {items.length === 0 ? (
        <Quiet says={searching ? "Nothing matches that." : "Nothing here for these filters."} />
      ) : (
        <Panel title="Notes and knowledge">
          <Rows label="Notes and knowledge">
            {items.map((item) => (
              <ItemRow
                key={`${item.kind}:${item.kind === "note" ? item.note.id : item.known.id}`}
                item={item}
                onSelect={onSelect}
                deciding={deciding}
                onApprove={(id) => approve.mutate(id)}
                onReject={(id) => reject.mutate(id)}
              />
            ))}
          </Rows>
        </Panel>
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
  onSelect(item: string): void;
  deciding: boolean;
  onApprove(proposalId: number): void;
  onReject(proposalId: number): void;
}

function ItemRow({ item, onSelect, deciding, onApprove, onReject }: ItemRowProps) {
  const ref =
    item.kind === "note"
      ? formatItem({ kind: "note", id: item.note.id })
      : formatItem({ kind: "knowledge", id: item.known.id });
  const title = item.kind === "note" ? firstLine(item.note.text) : item.known.title;
  const proposalId =
    item.kind === "knowledge" && item.known.status === "proposed" ? item.known.proposal_id : null;
  return (
    <Row className="unified-row">
      <button type="button" className="unified-open" onClick={() => onSelect(ref)}>
        <span className="unified-kind">{item.kind}</span>
        <span className="unified-title">{title}</span>
        <span className="unified-when">
          <RelativeTime at={itemDate(item)} />
        </span>
      </button>
      {proposalId !== null && (
        <>
          <Button variant="approve" aria-label={`Approve ${title}`} disabled={deciding} onClick={() => onApprove(proposalId)}>
            ✓
          </Button>
          <Button aria-label={`Refuse ${title}`} disabled={deciding} onClick={() => onReject(proposalId)}>
            ✗
          </Button>
        </>
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
