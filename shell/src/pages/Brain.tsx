import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { X } from "lucide-react";
import { BrainPanel } from "../brain/BrainPanel";
import { CapturePanel } from "../brain/CapturePanel";
import { CapturesWaiting } from "../brain/CapturesWaiting";
import { KnowledgePanel } from "../brain/knowledge/KnowledgePanel";
import { WaitingPanel } from "../brain/knowledge/WaitingPanel";
import { UnifiedList } from "../brain/UnifiedList";
import { ForceGraph } from "../brain/ForceGraph";
import { buildModel } from "../brain/graph-model";
import type { GFilters } from "../brain/graph-types";
import { GraphFilters, defaultGraphFilters } from "../brain/GraphFilters";
import { formatItem, itemOfNode, nodeIdOf, parseItem, parseView, type BrainView, type ItemRef } from "../brain/item-ref";
import { Segments } from "../brain/Segments";
import { useOpenCaptures } from "../data/captures";
import { useKnowledge, type Known } from "../data/knowledge";
import { useNotesGraph, useCreateNote, useOwnerNotes } from "../data/owner-notes";
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
  const closeItem = () =>
    void navigate({
      to: "/brain",
      search: (prev: Record<string, unknown>) => brainSearch({ ...validateBrainSearch(prev), item: undefined }),
    });

  return (
    <>
      <PageHeader
        title="Brain"
        headline="your notes and what the agent has been taught"
        actions={
          <div className="brain-head">
            <Tally />
            <Segments label="View" value={view} options={VIEWS} onChange={showView} />
          </div>
        }
      />

      <div className="brain-page">
        <Capture />
        {view === "graph" ? (
          <BrainGraphView item={item} onItem={showItem} />
        ) : (
          <BrainListView item={item} onItem={showItem} onClose={closeItem} />
        )}
      </div>
    </>
  );
}

/**
 * The page's answer to "is everything fine?", in one line: how many notes, how many lessons the
 * agent has in force, and what is held for the owner — each named by its noun, because the queue's
 * own phrase belongs to the Waiting page alone (`one-waiting-phrase.test.ts`). A figure
 * that has not loaded is a dash, never a zero — a zero would claim an answer the daemon has not given.
 */
function Tally() {
  const notes = useOwnerNotes("all");
  const knowledge = useKnowledge();
  const asked = useOpenCaptures();
  const count = <T,>(rows: T[] | undefined, keep: (row: T) => boolean) =>
    rows === undefined ? "—" : String(rows.filter(keep).length);
  const proposed = knowledge.data?.filter((row) => row.status === "proposed").length ?? 0;
  const open = asked.data?.length ?? 0;
  return (
    <p className="brain-tally">
      <span>
        <b>{count(notes.data, (note) => note.state === "active")}</b> notes
      </span>
      <span>
        <b>{count(knowledge.data, (row) => row.status === "active")}</b> lessons
      </span>
      {open > 0 && (
        <span className="brain-tally-asks">
          <b>{open}</b> {open === 1 ? "question" : "questions"}
        </span>
      )}
      {proposed > 0 && (
        <span className="brain-tally-asks">
          <b>{proposed}</b> to approve
        </span>
      )}
    </p>
  );
}

/** The save shortcut in the platform's own words: ⌘ Return on a Mac, Ctrl Enter elsewhere. */
const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

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
    <form
      className="brain-compose"
      aria-label="Capture"
      onSubmit={(event) => {
        event.preventDefault();
        save();
      }}
    >
      <label className="sr-only" htmlFor="brain-capture-text">
        Capture a note
      </label>
      <textarea
        id="brain-capture-text"
        ref={areaRef}
        className="brain-compose-input"
        rows={2}
        placeholder="Write it down — a fact, a reminder, a loose end. No agent reads it until you teach it."
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            save();
          }
        }}
      />
      <div className="brain-compose-foot">
        {create.isError ? (
          <ErrorNote>the núcleo did not answer — the note was not saved</ErrorNote>
        ) : (
          <span className="brain-compose-hint" aria-hidden="true">
            <kbd>{IS_MAC ? "⌘" : "Ctrl"}</kbd> <kbd>{IS_MAC ? "Return" : "Enter"}</kbd> saves
          </span>
        )}
        <Button type="submit" variant="approve" disabled={blank || create.isPending}>
          Save
        </Button>
      </div>
    </form>
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
  const canvasHeight = useCanvasHeight();

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
    <div className="brain-desk">
      <div className="brain-desk-main">
        <GraphFilters filters={filters} onFilters={setFilters} />
        {missing !== null && <Quiet says={missing} />}
        <ForceGraph model={model} selected={selected} onSelect={select} height={canvasHeight} />
      </div>
      <aside className="brain-aside" aria-label="Selected">
        {ref?.kind === "knowledge" && entity?.under !== item ? (
          <KnowledgePanel key={ref.id} id={ref.id} onSelect={select} />
        ) : (
          <BrainPanel nodeId={selected} graph={graph.data} model={model} onSelect={select} />
        )}
      </aside>
    </div>
  );
}

/**
 * The List side of the switch: the unified list, and beside it the aside. With nothing open the
 * aside holds what is asked of the owner — the daemon's questions and the lessons to approve — so
 * the list starts at the top of the page instead of under them; with an item open it holds that
 * item's panel, and Close (or Escape) gives the queue back. A knowledge row opens the knowledge
 * panel; a note opens the note panel, which reads the archived-inclusive notes graph.
 */
export function BrainListView({
  item,
  onItem,
  onClose,
}: {
  item?: string;
  onItem: (item: string) => void;
  onClose?: () => void;
}) {
  const ref = parseItem(item);
  const selected = ref === null ? undefined : formatItem(ref);
  const isOpen = ref !== null;
  const asked = useOpenCaptures();
  const knowledge = useKnowledge();
  const proposed = (knowledge.data ?? EMPTY_KNOWLEDGE).filter((row) => row.status === "proposed");
  const hasAsks = (asked.data?.length ?? 0) > 0 || proposed.length > 0;

  useEffect(() => {
    if (!isOpen || onClose === undefined) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isOpen, onClose]);

  return (
    <div className="brain-desk">
      <div className="brain-desk-main">
        <UnifiedList onSelect={onItem} selected={selected} />
      </div>
      <aside
        className={!isOpen && hasAsks ? "brain-aside brain-aside-asks" : "brain-aside"}
        aria-label={isOpen ? "Selected" : "For you"}
      >
        {ref !== null ? (
          <>
            {onClose !== undefined && (
              <div className="brain-aside-bar">
                <button type="button" className="brain-aside-close" onClick={onClose}>
                  <X aria-hidden="true" size={14} strokeWidth={2} />
                  Close
                </button>
              </div>
            )}
            <ItemPanel item={ref} onItem={onItem} />
          </>
        ) : hasAsks ? (
          <>
            <CapturesWaiting onSelect={onItem} selected={selected} />
            <WaitingPanel rows={proposed} />
          </>
        ) : (
          <div className="brain-aside-empty">
            <p className="brain-aside-empty-title">Nothing open</p>
            <p>Pick a row to read it whole, see what it links to, or teach it to the agent.</p>
          </div>
        )}
      </aside>
    </div>
  );
}

/**
 * The graph takes the window's height rather than a fixed 560px: on a tall window a fixed
 * canvas leaves a band of nothing under the map, and on 800x600 it pushes the panel off screen.
 * Read on resize; the canvas sets its own size through the CSSOM (`ForceGraph`).
 */
function useCanvasHeight(): number {
  const measure = () => Math.round(Math.min(900, Math.max(420, window.innerHeight - 320)));
  const [height, setHeight] = useState(measure);
  useEffect(() => {
    const onResize = () => setHeight(measure());
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return height;
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
