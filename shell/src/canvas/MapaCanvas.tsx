// §spec mapa-do-projeto
import { useMemo, useState } from "react";
import type { MapImport, MapModule } from "../data/project-map";
import { NODE_H, type Layout } from "./layered";
import { buildCommunities, buildCommunity, cellKey, moduleName } from "./map-graphs";

/**
 * How this project is built, as two nested pictures.
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
 * Nothing here is a `xyflow` canvas, unlike its two neighbours. Those lay out as they render; this
 * one already knows every coordinate before it draws, because the number that decides whether a
 * community is drawable has to be the number the drawing actually has. Handing the positions to a
 * library that may adjust them would put those two apart.
 */

export interface MapaCanvasProps {
  modules: MapModule[];
  imports: MapImport[];
}

export function MapaCanvas({ modules, imports }: MapaCanvasProps) {
  const [open, setOpen] = useState<string | null>(null);
  const matrix = useMemo(() => buildCommunities(modules, imports), [modules, imports]);
  const inside = useMemo(
    () => (open === null ? null : buildCommunity(matrix.members.get(open) ?? [], imports)),
    [open, matrix, imports],
  );

  if (matrix.order.length === 0) {
    return <p className="text-sm text-text-faint">Nothing here imports anything else.</p>;
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
          <Graph drawn={inside.drawn} />
        )}
        <ol className="flex flex-col gap-0.5">
          {members.map((path) => (
            <li key={path} className="font-mono text-[11px] text-text-muted">
              {path}
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

/** One community's files, laid out so a caller sits above what it calls. */
function Graph({ drawn }: { drawn: Layout }) {
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
            <g key={node.id} transform={`translate(${node.x - node.width / 2},${node.y - NODE_H / 2})`}>
              <title>{node.id}</title>
              <rect
                width={node.width}
                height={NODE_H}
                rx={4}
                className="fill-surface stroke-border"
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
