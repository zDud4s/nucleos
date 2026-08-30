// §spec mapa-do-projeto
import { useMemo, useState } from "react";
import type {
  Anchored,
  FileItems,
  ForeignFile,
  Junction,
  MapImport,
  MapModule,
  Standing,
} from "../data/project-map";
import { useFileItems } from "../data/project-map";
import { NODE_H, type Layout } from "./layered";
import { buildCommunities, buildCommunity, cellKey, moduleName } from "./map-graphs";
import { buildFileItems, fileFacts } from "./map-items";
import { claimedFiles, claimsFor, isSettled, standingLabel } from "./map-claims";
import { buildSeam, isBlindSpot, type Seam } from "./map-sides";

/**
 * How this project is built, as three nested pictures under one header.
 *
 * **The header is §16.4's L0 and it does not nest**, because the fact it carries is one no box
 * inside it can hold: this product has three sides that share no source, and a fourth piece —
 * `sidecars/`, 168 files — that nothing here can read at all. §16.5 forbids a descent that hides
 * a seam, so the seam stays on screen rather than behind a click somebody may never make.
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
  /**
   * What the walk found and no reader here understood, and the part of it that names a `§`.
   *
   * Needed by the top level and by nothing below it. §16.5 asks the top to carry the facts no
   * single box can hold, and *there is a side of this product nothing here can read* is the
   * largest of them: 168 files under `sidecars/`, which a picture drawn only from modules would
   * report as a project that does not have them.
   */
  unread: string[];
  foreign: ForeignFile[];
  /**
   * §16.4's L3, which is a verdict crossing every level rather than a level of its own.
   *
   * Read off the same answer the structure is, so the two cannot disagree about a project whose
   * folder moved between them — the reason the junction is drawn inside this component's parent
   * and not fetched again.
   */
  junction: Junction;
  standings: Record<string, Standing>;
}

export function MapaCanvas({
  projectId,
  modules,
  imports,
  unread,
  foreign,
  junction,
  standings,
}: MapaCanvasProps) {
  const [open, setOpen] = useState<string | null>(null);
  const [openFile, setOpenFile] = useState<string | null>(null);
  const matrix = useMemo(() => buildCommunities(modules, imports), [modules, imports]);
  const inside = useMemo(
    () => (open === null ? null : buildCommunity(matrix.members.get(open) ?? [], imports)),
    [open, matrix, imports],
  );
  const claimed = useMemo(() => claimedFiles(junction), [junction]);
  const seam = useMemo(
    () => buildSeam(modules, imports, unread, foreign),
    [modules, imports, unread, foreign],
  );

  if (matrix.order.length === 0) {
    return (
      <div className="flex flex-col gap-4">
        <Sides seam={seam} />
        <p className="text-sm text-text-faint">Nothing here imports anything else.</p>
      </div>
    );
  }

  if (openFile !== null) {
    return (
      <FileLevel
        projectId={projectId}
        path={openFile}
        back={open ?? "whole project"}
        onBack={() => setOpenFile(null)}
        claims={claimsFor(junction, openFile)}
        standings={standings}
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
              {claimed.has(path) ? null : (
                <span
                  className="ml-2 text-[10px] text-text-faint"
                  title="No approved decision names this file. Derived from the junction, and not a verdict about the code."
                >
                  nothing asked for it
                </span>
              )}
            </li>
          ))}
        </ol>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <Sides seam={seam} />
      <Matrix matrix={matrix} onOpen={setOpen} />
    </div>
  );
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
  claims,
  standings,
}: {
  projectId: string;
  path: string;
  back: string;
  onBack: () => void;
  claims: Anchored[];
  standings: Record<string, Standing>;
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
      <Claims claims={claims} standings={standings} />
    </div>
  );
}

/**
 * Which decisions claim this file, and what the owner has said about each.
 *
 * **§16.4's L3, read from the end a reader is standing at.** The spec describes a decision lighting
 * up the files under it; somebody looking at a file wants the same join backwards — *what was this
 * supposed to be, and is that still true?* It is the same data, so it needs no second query.
 *
 * **Two questions, drawn apart, because §5 forbids collapsing them.** *No decision names this file*
 * is derived and nobody said it; *this decision is stamped* is the owner's word and nothing else
 * can make it. One badge blending them would put the map and the owner in one voice.
 *
 * A decision is drawn below the picture rather than over it. The drawing answers *what is in here*;
 * this answers *was it wanted*, and §5 keeps those on separate axes rather than tinting one with
 * the other.
 */
function Claims({
  claims,
  standings,
}: {
  claims: Anchored[];
  standings: Record<string, Standing>;
}) {
  if (claims.length === 0) {
    return (
      <p className="max-w-prose text-xs text-text-faint">
        No approved decision names this file. That is §5.1&rsquo;s pile &mdash; code nobody asked
        for &mdash; and it is a fact about what has been declared, not a verdict about the code.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Decisions that claim this file
      </h3>
      <ul className="flex flex-col gap-1">
        {claims.map((claim) => {
          const standing = standings[String(claim.decision_id)];
          return (
            <li key={claim.decision_id} className="max-w-prose text-xs text-text-muted">
              <span className="font-mono text-[11px] text-text-faint">
                {claim.spec_slug} §{claim.section}
              </span>{" "}
              <span className={isSettled(standing) ? "text-accent" : "text-text-faint"}>
                {standingLabel(standing)}
              </span>
              <span className="ml-1 text-text">{claim.text}</span>
            </li>
          );
        })}
      </ul>
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
 * §16.4's L0 — the sides of the product, and what runs between them.
 *
 * **A header rather than a step you click into**, and that is §16.5's rule applied to the
 * navigation itself. *Descending may hide detail; it may never hide a seam* — so the seam is on
 * screen while the matrix is, instead of behind a click somebody may never make.
 *
 * **The zero is the hardest thing on this panel to draw honestly.** Three boxes with no line
 * between them is the truth and reads as independence, which is not. A Rust file cannot import a
 * TypeScript module in either direction, and the núcleo resolves `crate::` only inside a file's own
 * folder — so nothing crosses and nothing could. What the three islands say is that these sides
 * share no source. What passes between them is HTTP, and this map does not read it. That sentence
 * is printed rather than implied, because a number whose only possible value is zero looks like a
 * clean bill of health and is not one.
 */
function Sides({ seam }: { seam: Seam }) {
  // Two reasons a folder earns a line, and they are different facts. A folder with no box is a
  // piece of the product this map is blind to. A folder that has a box and still holds files
  // nothing reads is a side with something behind it — `core/`'s 131 are its SQL migrations, and 15
  // of them name a section. Only the repository root is left out when it cites nothing: five config
  // files are not a side of anything, and calling them one next to `sidecars/` flattens the
  // difference this level exists to draw.
  const quiet = seam.unread.filter(
    (folder) =>
      folder.citing > 0 || (isBlindSpot(seam, folder.folder) && folder.folder !== "(root)"),
  );
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        {seam.sides.map((side) => (
          <div
            key={side.key}
            className="flex min-w-[150px] flex-col gap-0.5 rounded-lg border border-border bg-surface px-3 py-2"
          >
            <span className="font-display text-sm text-text">{side.folder}</span>
            <span className="text-[10px] uppercase tracking-wide text-text-faint">{side.reader}</span>
            <span className="mt-1 text-xs text-text-muted">
              <span className="font-display text-sm text-text">{side.files}</span> file
              {side.files === 1 ? "" : "s"} · {side.imports} import{side.imports === 1 ? "" : "s"}{" "}
              inside
            </span>
            <span
              className="text-xs text-text-muted"
              title="Of the files naming a section, the ones whose header says which document it belongs to. Counted over citing files, as the header above is, so the two agree."
            >
              {side.citing === 0
                ? "nothing here cites a section"
                : `${side.declared} of ${side.citing} say which document`}
            </span>
            <span className="text-xs text-text-muted" title="Files with a test beside them.">
              {side.tested} have a test
            </span>
          </div>
        ))}
      </div>
      <p className="max-w-prose text-sm text-text-muted">
        {seam.crossing === 0 ? (
          <>
            <span className="text-text">No import crosses between them, and none could</span> — a
            Rust file cannot import a TypeScript module, and the núcleo resolves an import only
            inside the file&apos;s own folder. So this says these sides share no source; it does not
            say they are independent. What passes between them is HTTP, which this map does not
            read.
          </>
        ) : (
          <>
            <span className="text-danger">{seam.crossing}</span> import
            {seam.crossing === 1 ? "" : "s"} cross between sides. Nothing here should be able to do
            that, so the walk and this drawing disagree about what a side is.
          </>
        )}
        {seam.loose > 0 ? (
          <>
            {" "}
            <span className="text-danger">{seam.loose}</span> import
            {seam.loose === 1 ? " ends" : "s end"} on no file this map lists.
          </>
        ) : null}
      </p>
      {quiet.map((folder) => (
        <p key={folder.folder} className="max-w-prose text-sm text-text-muted">
          <span className="font-mono text-text">{folder.folder}/</span>{" "}
          {isBlindSpot(seam, folder.folder) ? (
            <>
              is <span className="text-text">{folder.files} files nothing here can read</span> — no
              box above is about it.
            </>
          ) : (
            <>
              also holds <span className="text-text">{folder.files} files nothing here can read</span>
              , behind the box that is about it.
            </>
          )}{" "}
          {folder.citing === 0
            ? "None of them names a section, so nothing can be said about what they were asked to do."
            : `${folder.citing} of them name a section, and ${folder.declared} say which document.`}
        </p>
      ))}
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
