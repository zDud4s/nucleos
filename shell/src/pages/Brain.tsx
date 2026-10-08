import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { BrainPanel } from "../brain/BrainPanel";
import { CapturePanel } from "../brain/CapturePanel";
import { KnowledgePanel } from "../brain/knowledge/KnowledgePanel";
import { UnifiedList } from "../brain/UnifiedList";
import { ForceGraph } from "../brain/ForceGraph";
import { buildModel } from "../brain/graph-model";
import type { GFilters } from "../brain/graph-types";
import { GraphFilters, defaultGraphFilters } from "../brain/GraphFilters";
import { formatItem, itemOfNode, nodeIdOf, parseItem, parseView, type BrainView, type ItemRef } from "../brain/item-ref";
import { useKnowledge, type Known } from "../data/knowledge";
import { useNotesGraph, useCreateNote } from "../data/owner-notes";
import { useProjects } from "../data/system";
import { Button, ErrorNote, PageHeader, Quiet } from "../ui";
import "./brain.css";

/**
 * Brain — the owner's notes (which no agent ever reads) and what the agent has been taught.
 *
 * A capture box above one attention-ordered list of both; the `List | Graph` switch is where they
 * are drawn as a map. The view and the
 * selected item live in the address (`?view=`, `?item=`), so a reload or a shared link lands on the
 * same thing, and "back" walks from item to item.
 */
export function Brain() {
  const { view, item } = useBrainSearch();
  const navigate = useNavigate();

  // Switching view replaces the entry; picking an item pushes one (spec §3.2).
  const showView = (next: BrainView) =>
    void navigate({
      to: "/brain",
      search: (prev: Record<string, unknown>) => brainSearch({ ...validateBrainSearch(prev), view: next }),
      replace: true,
    });
  const showItem = (next: string) =>
    void navigate({
      to: "/brain",
      search: (prev: Record<string, unknown>) => brainSearch({ ...validateBrainSearch(prev), item: next }),
    });

  return (
    <>
      <PageHeader title="Brain" headline="your notes and what the agent has been taught" />

      <Capture />

      <div className="brain-filters">
        <div role="group" aria-label="View">
          {VIEWS.map(([value, label]) => (
            <Button
              key={value}
              variant="quiet"
              aria-pressed={view === value}
              onClick={() => showView(value)}
            >
              {label}
            </Button>
          ))}
        </div>
      </div>

      {view === "graph" ? (
        <BrainGraphView item={item} onItem={showItem} />
      ) : (
        <BrainListView item={item} onItem={showItem} />
      )}
    </>
  );
}

const VIEWS: readonly (readonly [BrainView, string])[] = [
  ["list", "List"],
  ["graph", "Graph"],
];

export interface BrainSearch {
  capture?: number;
  view: BrainView;
  /** `note:<id>` | `knowledge:<id>`, already checked by `parseItem`. */
  item?: string;
}

/**
 * The Brain's search params.
 *
 * `capture` is the stamp a shortcut puts in the address to say "take a note now": a number or
 * nothing, as `validateVoiceSearch` does for `talk` — anything else is the same as not asking.
 * `view` is `list` unless it says `graph`. `item` is kept only when `parseItem` accepts it, so a
 * malformed one (`knowledge:abc`) is dropped rather than opening a panel for nothing.
 */
export function validateBrainSearch(search: Record<string, unknown>): BrainSearch {
  const out: BrainSearch = { view: parseView(search.view) };
  const capture = typeof search.capture === "number" ? search.capture : Number(search.capture);
  if (Number.isFinite(capture) && search.capture !== undefined && search.capture !== "") {
    out.capture = capture;
  }
  const item = parseItem(search.item);
  if (item !== null) out.item = formatItem(item);
  return out;
}

/** What goes back into the address: no empty keys, and the default view left out. */
function brainSearch(search: BrainSearch): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (search.capture !== undefined) out.capture = search.capture;
  if (search.view !== "list") out.view = search.view;
  if (search.item !== undefined) out.item = search.item;
  return out;
}

function useBrainSearch(): BrainSearch {
  return validateBrainSearch(useSearch({ strict: false }) as Record<string, unknown>);
}

/**
 * The capture box. Ctrl/Cmd+Enter saves; the box clears only once the daemon has answered, so a
 * refused note is still there to be sent again.
 */
function Capture() {
  const create = useCreateNote();
  const [text, setText] = useState("");
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const navigate = useNavigate();
  const { capture } = useBrainSearch();
  const consumedRef = useRef<number | undefined>(undefined);

  // The stamp is removed from the address as it is consumed, so a reload does not refocus; the
  // consumed value is remembered because clearing the address is itself a navigation. Only the
  // stamp goes: the view and the item it was opened with stay.
  useEffect(() => {
    if (capture === undefined || capture === consumedRef.current) return;
    consumedRef.current = capture;
    void navigate({
      to: "/brain",
      search: (prev: Record<string, unknown>) =>
        brainSearch({ ...validateBrainSearch(prev), capture: undefined }),
      replace: true,
    });
    areaRef.current?.focus();
  }, [capture, navigate]);

  const blank = text.trim() === "";
  const save = () => {
    if (blank || create.isPending) return;
    create.mutate({ text, origin: "shell" }, { onSuccess: () => setText("") });
  };

  return (
    <div className="brain-capture">
      <label className="brain-capture-label" htmlFor="brain-capture-text">
        Capture a note
      </label>
      <textarea
        id="brain-capture-text"
        ref={areaRef}
        className="brain-capture-input"
        rows={3}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            save();
          }
        }}
      />
      <div className="brain-capture-foot">
        <Button variant="approve" onClick={save} disabled={blank || create.isPending}>
          Save
        </Button>
        {create.isError && <ErrorNote>the núcleo did not answer — the note was not saved</ErrorNote>}
      </div>
    </div>
  );
}

const EMPTY_KNOWLEDGE: Known[] = [];
const ALL_ARCHIVED: GFilters = { ...defaultGraphFilters(), showArchived: true };

/**
 * What the graph is drawn without, in words: only the notes graph failing is an error (spec §3.3);
 * knowledge or the project roster missing just leaves their part out.
 */
function missingSentence(
  knowledge: { isError: boolean; data: unknown },
  projects: { isError: boolean; data: unknown },
): string | null {
  const parts: string[] = [];
  if (knowledge.isError) parts.push("knowledge did not answer — its rows are not drawn");
  else if (knowledge.data === undefined) parts.push("knowledge is still loading — its rows are not drawn yet");
  if (projects.isError) parts.push("the project roster did not answer — projects show their ids");
  else if (projects.data === undefined) parts.push("the project roster is still loading — projects show their ids");
  if (parts.length === 0) return null;
  const sentence = parts.join("; ");
  return `${sentence.charAt(0).toUpperCase()}${sentence.slice(1)}.`;
}

/**
 * The Graph side of the switch: filters and canvas on the left, the selected node on the right.
 *
 * A note is selected through `?item=`. Knowledge rows and entity nodes are selected in component
 * state until the knowledge panel exists; that selection belongs to the item it was made under, so
 * "back" to another item drops it.
 */
export function BrainGraphView({ item, onItem }: { item?: string; onItem: (item: string) => void }) {
  const [filters, setFilters] = useState<GFilters>(defaultGraphFilters);
  const [entity, setEntity] = useState<{ id: string; under: string | undefined } | null>(null);
  const graph = useNotesGraph(filters.showArchived);
  const knowledge = useKnowledge();
  const projects = useProjects();
  const knownRows = knowledge.data ?? EMPTY_KNOWLEDGE;
  const projectRows = projects.data;

  const model = useMemo(
    () => (graph.data === undefined ? null : buildModel(graph.data, knownRows, projectRows, filters)),
    [graph.data, knownRows, projectRows, filters],
  );

  const ref = parseItem(item);
  const selected =
    entity !== null && entity.under === item ? entity.id : ref !== null ? nodeIdOf(ref) : null;
  // A capture request has no graph node: it selects nothing there (nodeIdOf is null).
  const select = (nodeId: string) => {
    const picked = itemOfNode(nodeId);
    if (picked !== null) {
      setEntity(null);
      onItem(formatItem(picked));
    } else {
      setEntity({ id: nodeId, under: item });
    }
  };

  if (graph.isError) return <ErrorNote>the núcleo did not answer — the graph is not known</ErrorNote>;
  if (graph.data === undefined || model === null) return <Quiet says="Loading the graph…" />;
  const missing = missingSentence(knowledge, projects);

  return (
    <div className="brain-graph">
      <div className="brain-graph-main">
        <GraphFilters filters={filters} onFilters={setFilters} />
        {missing !== null && <Quiet says={missing} />}
        <ForceGraph model={model} selected={selected} onSelect={select} />
      </div>
      {ref?.kind === "knowledge" && entity?.under !== item ? (
        <KnowledgePanel key={ref.id} id={ref.id} onSelect={select} />
      ) : (
        <BrainPanel nodeId={selected} graph={graph.data} model={model} onSelect={select} />
      )}
    </div>
  );
}

/**
 * The List side of the switch: the unified list, and beside it the selected item's panel. A
 * knowledge row opens the knowledge panel; a note opens the note panel, which reads the archived-
 * inclusive notes graph.
 */
export function BrainListView({ item, onItem }: { item?: string; onItem: (item: string) => void }) {
  const ref = parseItem(item);
  return (
    <div className={ref === null ? undefined : "brain-graph"}>
      <div className="brain-graph-main">
        <UnifiedList onSelect={onItem} />
      </div>
      {ref !== null && <ItemPanel item={ref} onItem={onItem} />}
    </div>
  );
}

function ItemPanel({ item, onItem }: { item: ItemRef; onItem: (item: string) => void }) {
  const graph = useNotesGraph(true);
  const knowledge = useKnowledge();
  const projects = useProjects();
  const model = useMemo(
    () =>
      graph.data === undefined
        ? null
        : buildModel(graph.data, knowledge.data ?? EMPTY_KNOWLEDGE, projects.data, ALL_ARCHIVED),
    [graph.data, knowledge.data, projects.data],
  );
  const select = (nodeId: string) => {
    const picked = itemOfNode(nodeId);
    if (picked !== null) onItem(formatItem(picked));
  };
  if (item.kind === "capture") return <CapturePanel key={item.id} id={item.id} onSelect={onItem} />;
  if (item.kind === "knowledge") return <KnowledgePanel key={item.id} id={item.id} onSelect={select} />;
  if (graph.isError) return <ErrorNote>the núcleo did not answer — the note is not known</ErrorNote>;
  if (graph.data === undefined || model === null) return <Quiet says="Loading…" />;
  return <BrainPanel nodeId={nodeIdOf(item)} graph={graph.data} model={model} onSelect={select} />;
}
