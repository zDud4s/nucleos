// §spec mapa-do-projeto
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { X } from "lucide-react";
import type { ProjectMap } from "../data/project-map";
import { IconButton } from "../ui";
import {
  drawColumn,
  neighbours,
  seamSummary,
  type Atlas as AtlasModel,
  type Block,
  type Column,
  type GraphLink,
  type GraphNode,
} from "./atlas-model";

/**
 * The structure layer as an architecture drawing — what is in the folder, and what leans on what.
 *
 * One graph per side, one node per folder, layered so that each row uses the rows below it: the
 * entry points on top, the foundations at the bottom, every import between two folders a line,
 * thicker the more imports it carries. Choosing a node lights what it uses and what uses it, dims
 * the rest, and opens the folder in a panel over the canvas.
 *
 * **A canvas and not a column of the page.** The drawing is wider and taller than any page it sits
 * in, and laid inline it was cut, wrapped and scrolled past. It now fills the screen below the tabs
 * and moves instead: drag to pan, scroll to zoom, and it opens fitted to the frame.
 */
export function Atlas({ data, atlas }: { data: ProjectMap; atlas: AtlasModel }) {
  const seam = seamSummary(data.seam);
  const [chosen, setChosen] = useState<string | "seam" | null>(null);
  /** The folder under the pointer or the keyboard focus: it lights its lines without opening. */
  const [pointed, setPointed] = useState<string | null>(null);

  const near = useMemo(() => nearOf(atlas, chosen), [atlas, chosen]);
  /*
    The lines are faint until a folder is pointed at or chosen — at rest a side with fifteen
    folders is a skein, and every line drawn at full weight said nothing about any one of them.
  */
  const lit = pointed ?? (near === null ? null : chosen);
  const litNear = useMemo(
    () => (lit === chosen ? near : nearOf(atlas, lit)),
    [atlas, lit, chosen, near],
  );

  if (atlas.columns.length === 0) {
    return (
      <div className="ws-group ws-empty">
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <p className="m-0 text-sm font-medium text-text">No code this map can read</p>
          <p className="m-0 text-sm text-text-muted">
            The map reads Rust and TypeScript. What it could not read is under &ldquo;Not
            read&rdquo;.
          </p>
        </div>
      </div>
    );
  }

  const toggle = (key: string) => setChosen((was) => (was === key ? null : key));
  const [first, ...rest] = atlas.columns;
  const columnView = (column: Column) => (
    <ColumnView
      key={column.key}
      column={column}
      edges={atlas.edges}
      lit={lit}
      chosen={chosen}
      usesOf={litNear?.usesOf}
      usedByOf={litNear?.usedByOf}
      onChoose={toggle}
      onPoint={setPointed}
    />
  );
  const chosenBlock = chosen !== null && chosen !== "seam" ? atlas.blocks.get(chosen) : undefined;
  const panel =
    chosenBlock !== undefined && near !== null ? (
      <FolderDetail block={chosenBlock} uses={near.uses} usedBy={near.usedBy} onChoose={toggle} />
    ) : chosen === "seam" && data.seam !== undefined ? (
      <SeamDetail data={data} />
    ) : null;

  return (
    <Canvas fitKey={atlas} panel={panel} onClose={() => setChosen(null)}>
      <div className="ws-atlas">
        {first === undefined ? null : columnView(first)}
        {/* The seam sits between the two largest sides, which is where the calls cross. */}
        {rest.length > 0 && seam !== null ? (
          <button
            type="button"
            className="ws-atlas-seam"
            aria-pressed={chosen === "seam"}
            onClick={() => toggle("seam")}
          >
            <span className="ws-atlas-seam-kind">HTTP</span>
            <span className="ws-atlas-seam-figure">{seam.routes}</span>
            <span>routes</span>
            <span className="ws-atlas-seam-figure">{seam.calls}</span>
            <span>calls</span>
            {seam.broken > 0 ? (
              <span className="ws-atlas-broken">{seam.broken} broken</span>
            ) : (
              <span>none broken</span>
            )}
          </button>
        ) : null}
        {/*
          The smaller sides stack beside the largest rather than wrapping under it: the Tauri host's
          eight files fit under the core, and wrapped they sat alone under a tall drawing.
        */}
        {rest.length > 0 ? <div className="ws-atlas-rest">{rest.map(columnView)}</div> : null}
      </div>
    </Canvas>
  );
}

/** The canvas never shrinks below this, so a short window still shows a usable drawing. */
const MIN_HEIGHT = 420;
const MIN_ZOOM = 0.2;
const MAX_ZOOM = 2.5;

/**
 * A frame that fills the window below it, with a world inside that pans and zooms.
 *
 * The world keeps its natural size (`offsetWidth` ignores transforms), so fitting is one division.
 * A press that starts on a control — a node, the seam, the tools, the panel — is that control's;
 * only one on the empty ground drags.
 */
function Canvas({
  fitKey,
  panel,
  onClose,
  children,
}: {
  /** Refits whenever this changes: a new reading is a new drawing. */
  fitKey: unknown;
  panel: ReactNode;
  onClose: () => void;
  children: ReactNode;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const world = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; y: number } | null>(null);
  const [height, setHeight] = useState(MIN_HEIGHT);
  const [view, setView] = useState({ x: 0, y: 0, k: 1 });

  // Down to the bottom of the window, whatever sits above the canvas.
  useLayoutEffect(() => {
    const measure = () => {
      const f = frame.current;
      if (!f) return;
      const scroller = scrollParent(f);
      const top = f.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
      setHeight(Math.max(MIN_HEIGHT, Math.floor(scroller.clientHeight - top - 16)));
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);

  /*
    Whatever pads the page under the canvas would still make it scroll by that much: once drawn at
    the full room, take off exactly the overflow. It only ever shrinks, so it settles in one pass.
  */
  useLayoutEffect(() => {
    const f = frame.current;
    if (!f) return;
    const scroller = scrollParent(f);
    const over = scroller.scrollHeight - scroller.clientHeight;
    if (over > 0 && height > MIN_HEIGHT) setHeight(Math.max(MIN_HEIGHT, height - over));
  }, [height]);

  const fit = useCallback(() => {
    const f = frame.current;
    const w = world.current;
    if (!f || !w || !w.offsetWidth || !f.clientWidth) return;
    const k = Math.min(
      1,
      (f.clientWidth - 48) / w.offsetWidth,
      (f.clientHeight - 72) / w.offsetHeight,
    );
    const scale = Math.max(MIN_ZOOM, k);
    setView({
      k: scale,
      x: Math.max(24, (f.clientWidth - w.offsetWidth * scale) / 2),
      y: Math.max(24, (f.clientHeight - w.offsetHeight * scale) / 2),
    });
  }, []);
  useLayoutEffect(fit, [fit, fitKey, height]);

  /** Zooms by `factor` about a point of the frame, so what is under it stays under it. */
  const zoomAt = useCallback((factor: number, px: number, py: number) => {
    setView((v) => {
      const k = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, v.k * factor));
      return { k, x: px - ((px - v.x) * k) / v.k, y: py - ((py - v.y) * k) / v.k };
    });
  }, []);
  const zoomCentre = (factor: number) => {
    const f = frame.current;
    if (f) zoomAt(factor, f.clientWidth / 2, f.clientHeight / 2);
  };

  // Not a React handler: React's wheel listener is passive, and the page must not scroll too.
  useEffect(() => {
    const f = frame.current;
    if (!f) return;
    const onWheel = (event: WheelEvent) => {
      if ((event.target as Element).closest(".ws-canvas-panel")) return;
      event.preventDefault();
      const r = f.getBoundingClientRect();
      zoomAt(Math.exp(-event.deltaY * 0.0015), event.clientX - r.left, event.clientY - r.top);
    };
    f.addEventListener("wheel", onWheel, { passive: false });
    return () => f.removeEventListener("wheel", onWheel);
  }, [zoomAt]);

  return (
    <div
      ref={frame}
      className="ws-canvas"
      style={{
        height,
        backgroundPosition: `${view.x}px ${view.y}px`,
        backgroundSize: `${24 * view.k}px ${24 * view.k}px`,
      }}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        const target = event.target as Element;
        if (target.closest('[role="button"], button, input, a, .ws-canvas-panel')) return;
        drag.current = { x: event.clientX - view.x, y: event.clientY - view.y };
        event.currentTarget.setPointerCapture?.(event.pointerId);
      }}
      onPointerMove={(event) => {
        const d = drag.current;
        if (d) setView((v) => ({ ...v, x: event.clientX - d.x, y: event.clientY - d.y }));
      }}
      onPointerUp={() => {
        drag.current = null;
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
    >
      <div
        ref={world}
        className="ws-canvas-world"
        style={{ transform: `translate(${view.x}px, ${view.y}px) scale(${view.k})` }}
      >
        {children}
      </div>

      <p className="ws-canvas-hint">
        Folders use the ones below them. Drag to move, scroll to zoom, choose a folder to open it.
      </p>

      <div className="ws-canvas-tools" role="group" aria-label="Zoom">
        <button type="button" aria-label="Zoom out" onClick={() => zoomCentre(1 / 1.25)}>
          −
        </button>
        <span className="ws-canvas-zoom">{Math.round(view.k * 100)}%</span>
        <button type="button" aria-label="Zoom in" onClick={() => zoomCentre(1.25)}>
          +
        </button>
        <button type="button" onClick={fit}>
          Fit
        </button>
      </div>

      {panel === null ? null : (
        <aside className="ws-canvas-panel">
          <span className="absolute top-2 right-2">
            <IconButton label="Close" icon={X} onClick={onClose} />
          </span>
          {panel}
        </aside>
      )}
    </div>
  );
}

/** The nearest ancestor that scrolls, else the document: the canvas fills what that one shows. */
function scrollParent(node: HTMLElement): HTMLElement {
  for (let at = node.parentElement; at !== null; at = at.parentElement) {
    const { overflowY } = getComputedStyle(at);
    if ((overflowY === "auto" || overflowY === "scroll") && at.clientHeight > 0) return at;
  }
  return (document.scrollingElement as HTMLElement | null) ?? document.documentElement;
}

/**
 * What the map could not follow: source in languages it does not read, by folder. Files that are
 * not code at all get one quiet line under it — they are not a blind spot, only not drawn.
 */
export function NotRead({ atlas }: { atlas: AtlasModel }) {
  const rest =
    atlas.notCode === 0 ? null : (
      <p className="m-0 text-sm text-text-faint">
        Not code, so not drawn: <span className="tabular-nums">{atlas.notCode}</span> files (
        {atlas.notCodeKinds.slice(0, 5).join(", ")}
        {atlas.notCodeKinds.length > 5 ? ", …" : ""}). Build output is left out.
      </p>
    );
  if (atlas.unread.length === 0) {
    return (
      <div className="flex flex-col gap-2">
        <p className="m-0 text-sm text-text-muted">
          The map reads every source file in this folder.
        </p>
        {rest}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="m-0 text-sm text-text-muted">
        <span className="tabular-nums">{atlas.unreadFiles}</span> source files in languages the map
        cannot follow, by folder.
      </p>
      <ul className="ws-atlas-unread">
        {atlas.unread.map((pile) => (
          <li key={pile.folder}>
            <span className="font-mono text-text">{pile.folder}</span>
            <span className="tabular-nums">{pile.files}</span>
            <span className="text-text-faint">{pile.kinds.slice(0, 2).join(" · ")}</span>
          </li>
        ))}
      </ul>
      {rest}
    </div>
  );
}

/** What one folder uses and what uses it, with lookups by key; `null` for no folder. */
function nearOf(atlas: AtlasModel, key: string | null) {
  if (key === null || !atlas.blocks.has(key)) return null;
  const { uses, usedBy } = neighbours(atlas, key);
  return {
    uses,
    usedBy,
    usesOf: new Map(uses.map(([block, count]) => [block.key, count])),
    usedByOf: new Map(usedBy.map(([block, count]) => [block.key, count])),
  };
}

type NodeState = "chosen" | "uses" | "used-by" | "far" | undefined;

function ColumnView({
  column,
  edges,
  lit,
  chosen,
  usesOf,
  usedByOf,
  onChoose,
  onPoint,
}: {
  column: Column;
  edges: Map<string, number>;
  /** The folder whose lines are drawn at full weight: pointed at, else chosen. */
  lit: string | null;
  chosen: string | null;
  usesOf: Map<string, number> | undefined;
  usedByOf: Map<string, number> | undefined;
  onChoose: (key: string) => void;
  onPoint: (key: string | null) => void;
}) {
  const drawing = useMemo(() => drawColumn(column, edges), [column, edges]);
  const stateOf = (key: string): NodeState =>
    chosen === key || lit === key
      ? "chosen"
      : usesOf?.has(key)
        ? "uses"
        : usedByOf?.has(key)
          ? "used-by"
          : usesOf !== undefined
            ? "far"
            : undefined;
  const edgeState = (link: GraphLink): NodeState =>
    usesOf === undefined
      ? undefined
      : link.from === lit
        ? "uses"
        : link.to === lit
          ? "used-by"
          : "far";

  return (
    <section aria-label={`${column.name} · ${column.reader}`} className="ws-atlas-col">
      <header className="ws-atlas-head">
        <span className="font-medium text-text">{column.name}</span>
        <span className="text-text-faint">{column.reader}</span>
        <span className="ml-auto tabular-nums text-text-muted">{column.files} files</span>
      </header>
      <svg
        className="ws-atlas-graph"
        viewBox={`0 0 ${drawing.width} ${drawing.height}`}
        width={drawing.width}
        height={drawing.height}
      >
        <g aria-hidden="true">
          {drawing.links.map((link) => (
            <path
              key={`${link.from}${link.to}`}
              className="ws-atlas-edge"
              d={link.path}
              data-up={link.up}
              data-state={edgeState(link)}
              strokeWidth={1 + Math.log2(link.count)}
            />
          ))}
        </g>
        {drawing.nodes.map((node) => (
          <FolderNode
            key={node.block.key}
            node={node}
            state={stateOf(node.block.key)}
            onChoose={onChoose}
            onPoint={onPoint}
          />
        ))}
      </svg>
    </section>
  );
}

/** Longest label a node holds at its widest before it is cut. */
const LABEL_ROOM = 23;

function FolderNode({
  node,
  state,
  onChoose,
  onPoint,
}: {
  node: GraphNode;
  state: NodeState;
  onChoose: (key: string) => void;
  onPoint: (key: string | null) => void;
}) {
  const { block, x, y, w, h } = node;
  const files = block.files.length;
  const label =
    block.label.length > LABEL_ROOM ? `${block.label.slice(0, LABEL_ROOM - 1)}…` : block.label;
  const meta = `${files} ${files === 1 ? "file" : "files"}`;
  return (
    <g
      role="button"
      tabIndex={0}
      className="ws-atlas-node"
      data-state={state}
      aria-pressed={state === "chosen"}
      aria-label={`${block.label}, ${meta}, ${block.tested} tested`}
      onClick={() => onChoose(block.key)}
      onPointerEnter={() => onPoint(block.key)}
      onPointerLeave={() => onPoint(null)}
      onFocus={() => onPoint(block.key)}
      onBlur={() => onPoint(null)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onChoose(block.key);
        }
      }}
    >
      <title>{`${block.label}: ${meta}, ${block.tested} tested`}</title>
      <rect x={x} y={y} width={w} height={h} rx={8} />
      <text className="ws-atlas-node-name" x={x + w / 2} y={y + 19} textAnchor="middle">
        {label}
      </text>
      <text className="ws-atlas-node-meta" x={x + w / 2} y={y + 34} textAnchor="middle">
        {meta}
      </text>
      {/* How much of the folder has a test beside it — the nearest thing to evidence the map has. */}
      {block.tested > 0 ? (
        <rect
          className="ws-atlas-node-bar"
          x={x + 8}
          y={y + h - 4}
          width={((w - 16) * block.tested) / files}
          height={2}
          rx={1}
        />
      ) : null}
    </g>
  );
}

function FolderDetail({
  block,
  uses,
  usedBy,
  onChoose,
}: {
  block: Block;
  uses: [Block, number][];
  usedBy: [Block, number][];
  onChoose: (key: string) => void;
}) {
  const [query, setQuery] = useState("");
  const shown = block.files.filter((file) => file.path.toLowerCase().includes(query.toLowerCase()));
  return (
    <section aria-label={`The folder ${block.label}`} className="ws-group ws-atlas-detail">
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h3 className="m-0 text-base font-semibold text-text">{block.label}</h3>
        <p className="m-0 text-sm text-text-muted">
          <span className="tabular-nums">{block.files.length}</span> files ·{" "}
          <span className="tabular-nums">{block.tested}</span> tested ·{" "}
          <span className="tabular-nums">{block.citing}</span> name a spec section
        </p>
      </header>

      <div className="ws-atlas-links">
        <Neighbours
          title="Uses"
          rows={uses}
          onChoose={onChoose}
          empty="Nothing in another folder."
        />
        <Neighbours
          title="Used by"
          rows={usedBy}
          onChoose={onChoose}
          empty="No other folder on this side."
        />
      </div>

      <div className="flex flex-col gap-2">
        {block.files.length > 8 ? (
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={`Find in ${block.files.length} files`}
            aria-label="Find a file in this folder"
            className="ws-atlas-find"
          />
        ) : null}
        <ul className="ws-atlas-files">
          {shown.map((file) => {
            const sections = [...new Set(file.cites.map((cite) => `§${cite.section}`))];
            return (
              <li key={file.path}>
                <span className="min-w-0 truncate font-mono text-text">
                  {file.path.split("/").pop()}
                </span>
                {sections.length > 0 ? (
                  <span className="truncate font-mono text-text-faint">
                    {sections.slice(0, 4).join(" ")}
                    {sections.length > 4 ? " …" : ""}
                  </span>
                ) : null}
                <span className={file.tested ? "ws-atlas-tested" : "ws-atlas-untested"}>
                  {file.tested ? "tested" : "no test"}
                </span>
              </li>
            );
          })}
          {shown.length === 0 ? <li className="text-text-faint">No file matches.</li> : null}
        </ul>
      </div>
    </section>
  );
}

function Neighbours({
  title,
  rows,
  onChoose,
  empty,
}: {
  title: string;
  rows: [Block, number][];
  onChoose: (key: string) => void;
  empty: string;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <p className="m-0 text-xs font-medium text-text-faint">{title}</p>
      {rows.length === 0 ? (
        <p className="m-0 text-sm text-text-faint">{empty}</p>
      ) : (
        <ul className="m-0 flex list-none flex-col p-0">
          {rows.map(([block, count]) => (
            <li key={block.key}>
              <button type="button" className="ws-atlas-link" onClick={() => onChoose(block.key)}>
                <span className="truncate">{block.label}</span>
                <span className="tabular-nums text-text-faint">
                  {count} {count === 1 ? "import" : "imports"}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function SeamDetail({ data }: { data: ProjectMap }) {
  const seam = data.seam!;
  return (
    <section aria-label="The boundary between the sides" className="ws-group ws-atlas-detail">
      <header className="flex flex-col gap-1">
        <h3 className="m-0 text-base font-semibold text-text">The boundary</h3>
        <p className="m-0 max-w-prose text-sm text-text-muted">
          The sides share no source; they talk over HTTP. The daemon serves{" "}
          <span className="tabular-nums">{seam.served.length}</span> routes and the screens make{" "}
          <span className="tabular-nums">{seam.calls}</span> calls, of which{" "}
          <span className="tabular-nums">{seam.matched}</span> reach a route.
        </p>
      </header>
      {seam.unmatched.length > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="m-0 text-xs font-medium text-tone-danger-fg">
            Calls to a route nobody serves
          </p>
          <ul className="ws-atlas-files">
            {seam.unmatched.map((call) => (
              <li key={`${call.file}:${call.line}`}>
                <span className="font-mono text-text">{call.path}</span>
                <span className="truncate font-mono text-text-faint">
                  {call.file}:{call.line}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="m-0 text-sm text-text">Every call that could be read reaches a route.</p>
      )}
      {seam.uncalled.length > 0 ? (
        <details className="ws-disclosure">
          <summary className="text-sm text-text-muted">
            {seam.uncalled.length} routes no screen calls: agents and sidecars may
          </summary>
          <ul className="ws-atlas-files mt-2">
            {seam.uncalled.map((route) => (
              <li key={route}>
                <span className="font-mono text-text">{route}</span>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </section>
  );
}
