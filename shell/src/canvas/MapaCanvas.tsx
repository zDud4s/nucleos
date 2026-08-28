// §spec mapa-do-projeto
import {
  BaseEdge,
  EdgeLabelRenderer,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getSmoothStepPath,
  type EdgeProps,
  type EdgeTypes,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
// Through the bundle, never a CDN — the Tauri window's CSP is `default-src 'self'`, so a
// stylesheet fetched from anywhere else is a blank canvas in the shipped app and a working one in
// the dev server. Same argument as `WorkflowCanvas`, and the same import.
import "@xyflow/react/dist/style.css";
import type { ForeignFile, MapImport, MapModule } from "../data/project-map";
import { useState } from "react";
import {
  buildDocuments,
  buildMap,
  filesOf,
  moduleTone,
  UNDECLARED,
  type DocumentEdgeData,
  type DocumentFlowEdge,
  type DocumentFlowNode,
  type DocumentNodeData,
  type MapFlowEdge,
  type MapFlowNode,
} from "./map-model";

/**
 * The project one level above the file: **a node is a document, and its files are the ones that
 * said so.**
 *
 * **This is the first drawing of this map that fits on a screen, and the reason is the header.**
 * The file-level picture is 264 nodes; grouping them by folder — the only other boundary this
 * repository has — was measured and fails in both directions, because `core/src` is a single
 * directory holding the whole núcleo and `shell/src/project/` holds files of four different
 * documents. `§spec` is a boundary the owner wrote, so this grouping is derived rather than
 * invented, and it does not rot when somebody adds a file.
 *
 * **What is true here is the sizes and the edges. Position is not.** Nodes are ordered by size and
 * laid on a grid so the picture is stable between reads, and nothing about where a document sits
 * says anything about it. A layout that looked meaningful and was not would be the false
 * confidence this whole feature exists against, drawn instead of written.
 *
 * **The pile that declares no document is drawn, and it is the point rather than the leftover.**
 * It is exactly the §8 debt — files naming a section without saying which document — and while it
 * is large every confirmation elsewhere on this screen rests on files that did say. Hiding it
 * would draw the project as more organised than it is.
 *
 * **What this drawing does not show, said here because the picture cannot say it:** it does not go
 * inside a document, so it says nothing about how one is built; it draws no edge for a Go file,
 * because nothing here knows what one imports, though those files do count toward a document's
 * size; and a weight is a count of imports and never a claim that they matter.
 */

/**
 * Module scope, for the reason `WorkflowCanvas` records: xyflow compares these by reference and
 * rebuilds every node when either changes, so declaring them inside the component is a full
 * remount on every render — intermittent rather than constant once the compiler memoises around
 * it, which is worse.
 */
const nodeTypes: NodeTypes = { documentNode: DocumentNode, mapNode: ModuleNode };
const edgeTypes: EdgeTypes = { documentEdge: DocumentEdgeLine, mapEdge: ModuleEdgeLine };

/**
 * Above this, an edge carries its number.
 *
 * **A threshold and not a filter: every edge is drawn.** Labelling all of them turns the picture
 * into a field of digits and labelling none throws away the one quantity here. The number is
 * stated in the caption below the canvas rather than left for somebody to infer, which is the same
 * rule the rest of this map keeps — a drawing that omits has to say what it omitted.
 */
const LABEL_FROM = 4;

/* ------------------------------------------------------------------ node -- */

function DocumentNode({ data }: NodeProps<DocumentFlowNode>) {
  const { slug, files, citing } = data as DocumentNodeData;
  const undeclared = slug === UNDECLARED;

  return (
    <>
      <Handle type="target" position={Position.Left} isConnectable={false} />
      <div
        // The longest slugs truncate at this width. The full name is on the element for a hover
        // and in the label for a screen reader, so nothing is only in the pixels.
        title={undeclared ? undefined : (slug ?? undefined)}
        aria-label={
          undeclared
            ? `${files} file${files === 1 ? "" : "s"} naming a section under no document`
            : `${slug}, ${files} file${files === 1 ? "" : "s"}`
        }
        className={[
          "flex w-[170px] flex-col gap-1 rounded-lg px-3 py-2 text-left",
          // Dashed and muted, the way `resto` is drawn everywhere else in this project: present,
          // counted, and plainly not a document somebody decided.
          undeclared
            ? "border-2 border-dashed border-border bg-surface-sunken"
            : "border-2 border-border bg-surface",
        ].join(" ")}
      >
        <span
          className={`truncate font-display text-sm ${undeclared ? "text-text-muted" : "text-text"}`}
        >
          {undeclared ? "no document" : slug}
        </span>
        <span className="text-[10px] uppercase tracking-wide text-text-faint">
          {files} file{files === 1 ? "" : "s"}
          {/*
            The second number only when it differs. A document whose files all name a section is
            the ordinary case, and printing `12 of 12` on every node would spend the reader's
            attention on the rows where nothing is happening.
          */}
          {citing === files ? "" : ` · ${citing} name a section`}
        </span>
      </div>
      <Handle type="source" position={Position.Right} isConnectable={false} />
    </>
  );
}

/* --------------------------------------------------------- node, level 2 -- */

/**
 * One file inside an open document.
 *
 * The tone is {@link moduleTone}'s, unchanged: *did anybody ask for this* is the same question one
 * level down, and giving it a second vocabulary here would be two answers on two screens.
 */
function ModuleNode({ data }: NodeProps<MapFlowNode>) {
  const module = (data as { module: MapModule }).module;
  const name = module.path.slice(module.path.lastIndexOf("/") + 1);
  const tone = moduleTone(module);

  return (
    <>
      <Handle type="target" position={Position.Left} isConnectable={false} />
      <div
        title={module.path}
        aria-label={`${module.path}, ${module.cites.length} citation${module.cites.length === 1 ? "" : "s"}`}
        className="flex w-[170px] flex-col rounded-md border-2 bg-surface px-2 py-1 text-left"
        style={{ borderColor: `var(--tone-${tone}-border)` }}
      >
        <span className="truncate font-mono text-[11px] text-text">{name}</span>
        <span className="truncate text-[10px] text-text-faint">{module.path}</span>
      </div>
      <Handle type="source" position={Position.Right} isConnectable={false} />
    </>
  );
}

function ModuleEdgeLine({
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
}: EdgeProps) {
  const [path] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });
  return <BaseEdge path={path} style={{ strokeWidth: 1, stroke: "var(--border)" }} />;
}

/* ------------------------------------------------------------------ edge -- */

function DocumentEdgeLine({
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
}: EdgeProps<DocumentFlowEdge>) {
  const weight = (data as DocumentEdgeData | undefined)?.weight ?? 1;
  const [path, labelX, labelY] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });

  return (
    <>
      <BaseEdge
        path={path}
        style={{
          // Width carries the count, capped so one heavy pair cannot drown the rest of the
          // picture. The cap is why the number is also printed above `LABEL_FROM`: past it the
          // stroke stops being able to say how much heavier a pair is.
          strokeWidth: Math.min(1 + weight * 0.35, 5),
          stroke: "var(--border-strong)",
        }}
      />
      {weight >= LABEL_FROM ? (
        <EdgeLabelRenderer>
          <span
            style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
            className="pointer-events-none absolute rounded-pill border border-border bg-surface px-1.5 py-0.5 text-[10px] text-text-muted"
          >
            {weight}
          </span>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}

/* ---------------------------------------------------------------- canvas -- */

export interface MapaCanvasProps {
  modules: MapModule[];
  foreign: ForeignFile[];
  imports: MapImport[];
}

function MapaSurface({ modules, foreign, imports }: MapaCanvasProps) {
  /**
   * Which document is open, or `null` for the whole project.
   *
   * **An object rather than the slug itself, because `null` is a document here** — the pile that
   * declares none is a box like any other and has to be openable. A bare `string | null` would
   * make *the undeclared pile* and *nothing is open* the same value, and the one screen where that
   * collapse shows up is the one screen it must not.
   */
  const [open, setOpen] = useState<{ slug: string | null } | null>(null);
  const model = buildDocuments(modules, foreign, imports);

  if (model.nodes.length === 0) {
    // §11's project without specs, and the sentence it insists on. An empty canvas would be the
    // cheapest lie in the document — it looks like a map with nothing in it rather than like a
    // project nobody has told anything yet.
    return (
      <p className="text-sm text-text-faint">
        Nothing in this project names a spec section yet, so there are no documents to draw.
      </p>
    );
  }

  const inside = open === null ? null : buildMap(filesOf(modules, open.slug), imports);
  const name = open === null ? null : (open.slug ?? "no document");
  // Counted rather than drawn: nothing here knows what a Go file imports, so drawing one inside a
  // document would put a node with no edges beside nodes whose edges mean something.
  const unread =
    open === null ? 0 : foreign.filter((file) => (file.spec ?? null) === open.slug).length;

  return (
    <div className="flex flex-col gap-2">
      {open === null ? null : (
        <div className="flex items-baseline gap-3">
          <button
            type="button"
            onClick={() => setOpen(null)}
            className="rounded-pill border border-border px-2 py-0.5 text-xs text-text-muted hover:text-text"
          >
            &larr; all documents
          </button>
          <span className="font-display text-sm text-text">{name}</span>
          <span className="text-xs text-text-faint">
            {inside?.nodes.length ?? 0} file{inside?.nodes.length === 1 ? "" : "s"} this reader
            follows
            {unread > 0
              ? ` · ${unread} more in a language it cannot read, drawn nowhere`
              : ""}
          </span>
        </div>
      )}
      <div className="h-[520px] w-full overflow-hidden rounded-lg border border-border bg-surface-sunken">
        {/*
          Both levels share one canvas, so the generics are written out: inferred from the
          first branch they would be the document level's, and the file level would not
          typecheck against them.
        */}
        <ReactFlow<DocumentFlowNode | MapFlowNode, DocumentFlowEdge | MapFlowEdge>
          key={open === null ? "documents" : `inside:${open.slug ?? ""}`}
          nodes={inside?.nodes ?? model.nodes}
          edges={inside?.edges ?? model.edges}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          onNodeClick={(_event, node) => {
            // Only the top level opens. A file has nowhere further to go yet, and a click that
            // silently does nothing is worse than one that was never offered.
            if (open !== null) return;
            const slug = (node.data as DocumentNodeData).slug;
            setOpen({ slug });
          }}
          /*
            Draggable, unlike the workflow canvas, and the difference is what the layout means. A
            workflow's layout IS the sequence, so a node moved by hand would say something false.
            Here position carries nothing, so moving one costs nothing and untangling the picture
            by hand is the only tool this drawing offers.
          */
          nodesDraggable
          nodesConnectable={false}
          elementsSelectable={false}
          deleteKeyCode={null}
          proOptions={{ hideAttribution: false }}
          fitView
        />
      </div>
      <p className="text-xs text-text-muted">
        {open === null ? (
          <>
            A box is a document and its files are the ones carrying its <code>§spec</code> header;
            a line is one document&rsquo;s files importing another&rsquo;s, thicker the more of them
            there are, numbered from {LABEL_FROM}. Click a box to open it. Where a box sits means
            nothing. Go files count toward a box and carry no lines, because nothing here can read
            what one imports.
          </>
        ) : (
          <>
            The files that declared this document, and the imports between them — an import leaving
            it is not drawn here, because this level is about how one document is built. A file
            outlined faintly is one that names no section: it is inside this document and nothing
            has asked for it.
          </>
        )}
      </p>
    </div>
  );
}

export function MapaCanvas(props: MapaCanvasProps) {
  return (
    <ReactFlowProvider>
      <MapaSurface {...props} />
    </ReactFlowProvider>
  );
}
