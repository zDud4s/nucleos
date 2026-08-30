// §spec mapa-do-projeto
import { useMemo, useState } from "react";
import type { FileItems, MapImport, MapModule } from "../data/project-map";
import { useFileItems } from "../data/project-map";
import { NODE_H, type Layout } from "./layered";
import { buildCommunities, buildCommunity, cellKey, moduleName } from "./map-graphs";
import { buildFileItems, fileFacts } from "./map-items";

/**
 * How this project is built, as three nested pictures.
 *
 * **The instrument changes with the density, and that is the whole design.** This repository is 253
 * files joined by 1033 dependencies — four a file, and node-link drawings stop being readable
 * somewhere near two and a half. The first version of this screen drew them anyway: measured
 * afterwards, 73% of its edges crossed a box they had nothing to do with, and one corridor carried
 * 78 lines. So the project as a whole is a **matrix**, which scales to hundreds of rows and puts
 * every dependency that points backwards below the diagonal, where it can be counted.
 *
 * Inside a community the graph is sparse — that is what being a community means — and it is drawn
 * as a layered picture. Where even that would not read, the reason is shown instead of the picture.
 * **A drawing nobody can follow is worse than a sentence saying why, because it still looks like an
 * answer** — and looking like an answer while being unreadable is the failure this whole map exists
 * to refuse.
 *
 * **Inside a file the same layered picture draws again, and it is the measurement that earned that
 * third step rather than the symmetry.** Items reference each other 0.90 times apiece at the median
 * and 1.65 at the 90th percentile; of 272 readable files, not one is over the 2.6 the drawing
 * refuses at. What forces a matrix at the top is simply absent at the bottom. The 29 files that do
 * refuse refuse on size — `http.rs` declares 385 things — and they refuse showing their numbers.
 *
 * **Descending hides detail and never a seam** (§16.5). Every level prints what it left out: the
 * matrix names the files it found no dependency for, and the file level carries the núcleo's own
 * count of the locals and nested functions its reader could not see. A picture that quietly drops
 * half of what it was given is the false confidence of §1 again, in pixels.
 *
 * Nothing here is a `xyflow` canvas, unlike its two neighbours. Those lay out as they render; this
 * one already knows every coordinate before it draws, because the number that decides whether a
 * community is drawable has to be the number the drawing actually has. Handing the positions to a
 * library that may adjust them would put those two apart.
 */

export interface MapaCanvasProps {
  /** Needed only by the file level, which is the one thing here that reads a route of its own. */
  projectId: string;
  modules: MapModule[];
  imports: MapImport[];
}

export function MapaCanvas({ projectId, modules, imports }: MapaCanvasProps) {
  const [open, setOpen] = useState<string | null>(null);
  const [openFile, setOpenFile] = useState<string | null>(null);
  const matrix = useMemo(() => buildCommunities(modules, imports), [modules, imports]);
  const inside = useMemo(
    () => (open === null ? null : buildCommunity(matrix.members.get(open) ?? [], imports)),
    [open, matrix, imports],
  );

  if (matrix.order.length === 0) {
    return <p className="text-sm text-text-faint">Nothing here imports anything else.</p>;
  }

  if (openFile !== null) {
    return (
      <FileLevel
        projectId={projectId}
        path={openFile}
        back={open ?? "whole project"}
        onBack={() => setOpenFile(null)}
      />
    );
  }

  if (open !== null && inside !== null) {
    const members = matrix.members.get(open) ?? [];
    return (
      <div className="flex flex-col gap-2">
        <div className="flex items-baseline gap-3">
          <button
            type="button"
            onClick={() => setOpen(null)}
            className="rounded-pill border border-border px-2 py-0.5 text-xs text-text-muted hover:text-text"
          >
            ← whole project
          </button>
          <span className="font-display text-sm text-text">{open}</span>
          <span className="text-xs text-text-faint">
            {members.length} file{members.length === 1 ? "" : "s"} ·{" "}
            {inside.links.length} import{inside.links.length === 1 ? "" : "s"} between them
          </span>
        </div>
        {inside.refused.length > 0 ? (
          <Refused reasons={inside.refused} />
        ) : (
          <Graph drawn={inside.drawn} onOpen={setOpenFile} />
        )}
        <ol className="flex flex-col gap-0.5">
          {members.map((path) => (
            <li key={path}>
              <button
                type="button"
                onClick={() => setOpenFile(path)}
                className="font-mono text-[11px] text-text-muted hover:text-text"
              >
                {path}
              </button>
            </li>
          ))}
        </ol>
      </div>
    );
  }

  return <Matrix matrix={matrix} onOpen={setOpen} />;
}

/** The honest half: why a picture is not being drawn, in the numbers that decided it. */
function Refused({ reasons }: { reasons: string[] }) {
  return (
    <div className="rounded-lg border border-border bg-surface-sunken px-4 py-3">
      <p className="text-sm text-text">This one does not draw, and pretending otherwise would help nobody.</p>
      <ul className="mt-1 list-disc pl-5 text-xs text-text-muted">
        {reasons.map((reason) => (
          <li key={reason}>{reason}</li>
        ))}
      </ul>
    </div>
  );
}

/**
 * One file's own declarations, read from the route that answers about a single file.
 *
 * **Its own query and not a slice of the map's.** `GET /map` walks the whole tree, asks git about
 * every anchor and reads three tables; carrying every file's items on it would multiply the largest
 * answer the daemon sends by the size of the project, for a level nobody looks at until they click
 * into it. So this reads when a reader points at something — which is also why it is a component of
 * its own: it is mounted only once there is a file to ask about.
 */
function FileLevel({
  projectId,
  path,
  back,
  onBack,
}: {
  projectId: string;
  path: string;
  back: string;
  onBack: () => void;
}) {
  const found = useFileItems(projectId, path);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-baseline gap-3">
        <button
          type="button"
          onClick={onBack}
          className="rounded-pill border border-border px-2 py-0.5 text-xs text-text-muted hover:text-text"
        >
          ← {back}
        </button>
        <span className="font-mono text-xs text-text">{path}</span>
      </div>
      {found.isError ? (
        <p className="text-sm text-text-faint">
          The núcleo could not read this file — it may have moved since the map was walked.
        </p>
      ) : found.data === undefined ? (
        <p className="text-sm text-text-faint">Reading the file…</p>
      ) : (
        <FileDrawing found={found.data} />
      )}
    </div>
  );
}

/**
 * What one file declares, drawn.
 *
 * **The numbers are the point of this level and the drawing is how they are reached.** *Está como
 * eu queria?* is not answered by a shape; it is answered by knowing what this file offers the rest
 * of the project, how much of it is explained, and what nothing reaches. The picture is what makes
 * those countable at a glance.
 */
function FileDrawing({ found }: { found: FileItems }) {
  const drawing = useMemo(() => buildFileItems(found), [found]);
  const facts = useMemo(() => fileFacts(found), [found]);
  const reached = useMemo(() => new Set(found.references.map((edge) => edge.to)), [found]);
  const byId = useMemo(() => new Map(found.items.map((item) => [item.id, item])), [found]);

  if (found.reader === null) {
    return (
      <p className="text-sm text-text-faint">
        Nothing here reads this language yet, so this file has no declarations to draw. It is not
        missing from the map — the level above counts it, and says the same thing about it.
      </p>
    );
  }

  if (found.items.length === 0) {
    return (
      <p className="text-sm text-text-faint">
        This file declares nothing of its own. It is imports, or a list of modules, or both.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-x-6 gap-y-1 text-xs text-text-muted">
        <span>
          <span className="font-display text-sm text-text">{facts.items}</span> declaration
          {facts.items === 1 ? "" : "s"}
        </span>
        <span>
          <span className="font-display text-sm text-text">{facts.exported}</span> reachable from
          outside
        </span>
        <span>
          <span className="font-display text-sm text-text">{facts.documented}</span> with a doc
          comment
        </span>
        {facts.unreachable > 0 ? (
          <span title="Nothing in this file uses it, and nothing outside it can. Trait methods, tests and components named only by a route are reachable in ways this reader cannot see.">
            <span className="font-display text-sm text-text">{facts.unreachable}</span> nothing here
            reaches
          </span>
        ) : null}
      </div>
      {drawing.refused.length > 0 ? (
        <Refused reasons={drawing.refused} />
      ) : (
        <Graph
          drawn={drawing.drawn}
          accent={(id) => {
            const item = byId.get(id);
            if (item === undefined) return "fill-surface stroke-border";
            if (!item.exported && !reached.has(item.id)) {
              return "fill-surface-sunken stroke-border-subtle";
            }
            return item.exported ? "fill-surface stroke-accent" : "fill-surface stroke-border";
          }}
        />
      )}
      {/*
        §16.5 one floor down: descending may hide detail and must never hide a seam. These are the
        núcleo's own words for what its reader could not see in this file — locals, and functions
        nested inside other functions. Printed rather than logged, because a file drawn as four
        boxes when it declares forty things has told its owner something false.
      */}
      {found.missed.length > 0 ? (
        <p className="text-xs text-text-faint">Not drawn: {found.missed.join("; ")}.</p>
      ) : null}
      <ol className="flex flex-col gap-0.5">
        {found.items.map((item) => (
          <li key={item.id} className="font-mono text-[11px] text-text-muted">
            <span className="text-text-faint">{item.line}</span> {item.id}
            {item.exported ? <span className="text-accent"> ·pub</span> : null}
            {item.documented ? null : <span className="text-text-faint"> ·undocumented</span>}
          </li>
        ))}
      </ol>
    </div>
  );
}

/**
 * The dependencies between communities as a grid.
 *
 * Rows are ordered so as little as possible falls below the diagonal — the ordering by depth this
 * replaced put 146 of 372 dependencies down there where a better one puts 84, so more than half of
 * the arrows a reader saw pointing backwards were the sort's fault and not the code's. **What is
 * left below the diagonal after this is coupling no arrangement removes**, which is the only form
 * of that number worth acting on.
 */
function Matrix({
  matrix,
  onOpen,
}: {
  matrix: ReturnType<typeof buildCommunities>;
  onOpen: (title: string) => void;
}) {
  const total = matrix.back + matrix.forward;
  const share = total === 0 ? 0 : Math.round((100 * matrix.back) / total);
  return (
    <div className="flex flex-col gap-3">
      <p className="max-w-prose text-sm text-text-muted">
        {matrix.files} files joined by {matrix.deps} dependencies — {(matrix.deps / matrix.files).toFixed(1)}{" "}
        each. No arrangement of boxes and arrows survives that, so this is a matrix: each row uses
        the columns marked in it. The files were grouped into {matrix.order.length} communities found
        from the imports themselves. <span className="text-text">A mark above the diagonal is a
        dependency that goes down. One below points backwards</span> — and no reordering removes it.
      </p>
      <div className="flex flex-wrap gap-x-6 gap-y-1 text-xs text-text-muted">
        <span>
          <span className="font-display text-sm text-text">{matrix.forward}</span> forwards
        </span>
        <span>
          <span className="font-display text-sm text-text">{matrix.back}</span> backwards · {share}%
        </span>
        {matrix.alone.length > 0 ? (
          <span title={matrix.alone.join("\n")}>
            <span className="font-display text-sm text-text">{matrix.alone.length}</span> with no
            dependency either way
          </span>
        ) : null}
      </div>
      <div className="max-h-[560px] w-full overflow-auto rounded-lg border border-border bg-surface p-3">
        <table className="border-collapse font-mono text-[10px]">
          <thead>
            <tr>
              <th />
              {matrix.order.map((title) => (
                <th key={title} className="h-28 align-bottom pb-1">
                  <button
                    type="button"
                    onClick={() => onOpen(title)}
                    className="[writing-mode:vertical-rl] rotate-180 text-text-muted hover:text-text"
                  >
                    {title}
                  </button>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {matrix.order.map((row, i) => (
              <tr key={row}>
                <th className="whitespace-nowrap px-1 text-right font-normal">
                  <button
                    type="button"
                    onClick={() => onOpen(row)}
                    className="text-text-muted hover:text-text"
                  >
                    {row}{" "}
                    <span className="text-text-faint">{matrix.members.get(row)?.length}</span>
                  </button>
                </th>
                {matrix.order.map((column, j) => {
                  const weight = matrix.cells.get(cellKey(row, column));
                  const tone =
                    i === j
                      ? "bg-surface-sunken"
                      : weight === undefined
                        ? ""
                        : j > i
                          ? "bg-accent/30"
                          : "bg-danger/30";
                  return (
                    <td
                      key={column}
                      title={weight === undefined ? undefined : `${row} uses ${column} — ${weight}`}
                      className={`h-[19px] w-[19px] border border-border-subtle text-center ${tone}`}
                    >
                      {weight ?? ""}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

const PAD = 26;

/**
 * A layered drawing, whatever its boxes happen to be.
 *
 * **It does not know which level it is drawing**, which is what let the file level exist without a
 * second renderer. A box is an id, a label and a position; whether that id is a path or a
 * `Type::method` is the caller's business. `onOpen` is what makes a box a door — the community
 * level hands one in, and the file level, having nothing below it, does not.
 */
function Graph({
  drawn,
  onOpen,
  accent,
}: {
  drawn: Layout;
  onOpen?: (id: string) => void;
  accent?: (id: string) => string;
}) {
  const at = new Map(drawn.nodes.map((node) => [node.id, { x: node.x, y: node.y }]));
  for (const [id, where] of Object.entries(drawn.bends)) at.set(id, where);
  const isBend = (id: string) => drawn.bends[id] !== undefined;

  return (
    <div className="max-h-[560px] w-full overflow-auto rounded-lg border border-border bg-surface">
      <svg
        width={drawn.width + PAD * 2}
        height={drawn.height + PAD * 2}
        viewBox={`${-PAD} ${-PAD + NODE_H / 2} ${drawn.width + PAD * 2} ${drawn.height + PAD * 2}`}
      >
        <g>
          {drawn.segments.map((segment) => {
            const a = at.get(segment.from);
            const b = at.get(segment.to);
            if (!a || !b) return null;
            const y1 = a.y + (isBend(segment.from) ? 0 : NODE_H / 2);
            const y2 = b.y - (isBend(segment.to) ? 0 : NODE_H / 2);
            const mid = (y1 + y2) / 2;
            return (
              <path
                key={`${segment.from}>${segment.to}`}
                d={`M${a.x} ${y1} C${a.x} ${mid} ${b.x} ${mid} ${b.x} ${y2}`}
                fill="none"
                className={segment.reversed ? "stroke-danger" : "stroke-border"}
                strokeWidth={1.2}
                strokeDasharray={segment.reversed ? "4 3" : undefined}
              />
            );
          })}
        </g>
        <g>
          {drawn.nodes.map((node) => (
            <g
              key={node.id}
              transform={`translate(${node.x - node.width / 2},${node.y - NODE_H / 2})`}
              onClick={onOpen === undefined ? undefined : () => onOpen(node.id)}
              className={onOpen === undefined ? undefined : "cursor-pointer"}
            >
              <title>{node.id}</title>
              <rect
                width={node.width}
                height={NODE_H}
                rx={4}
                className={accent === undefined ? "fill-surface stroke-border" : accent(node.id)}
                strokeWidth={1.2}
              />
              {node.lines.map((line, index) => (
                <text
                  key={line}
                  x={node.width / 2}
                  y={(node.lines.length > 1 ? 15 : 21) + index * 12}
                  textAnchor="middle"
                  className="fill-text font-mono text-[11px]"
                >
                  {line}
                </text>
              ))}
            </g>
          ))}
        </g>
      </svg>
    </div>
  );
}

export { moduleName };
