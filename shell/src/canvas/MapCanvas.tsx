// §spec mapa-do-projeto
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type {
  Anchored,
  FileItems,
  Junction,
  MapImport,
  MapModule,
  Standing,
} from "../data/project-map";
import { useFileItems } from "../data/project-map";
import { SectionTitle } from "../ui";
import { NODE_H, type Layout } from "./layered";
import {
  buildCommunities,
  buildCommunity,
  cellKey,
  moduleName,
  neighbourCount,
  sliceAround,
  trafficFor,
} from "./map-graphs";
import { buildFileItems, fileFacts } from "./map-items";
import { claimedFiles, claimsFor, isSettled, standingLabel } from "./map-claims";
import { ASSUMED_ROOM, fitZoom, matrixWidth, unreadableAt, zoomBy, zoomLabel } from "./map-zoom";

/**
 * How this project is built, as three nested pictures under one header.
 *
 * **The header is §16.4's L0 and it is not here**, because the fact it carries is one no box
 * inside this can hold — this product has three sides that share no source, and a fourth piece,
 * `sidecars/`, that nothing here can read at all. {@link Boundary} says it, one level up, above
 * the row of views: §16.5 forbids a descent that hides a seam, and the picture is now one of five
 * things the mode draws over the same answer, so a header belonging to the picture alone would be
 * a seam behind a click for the other four.
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

export interface MapCanvasProps {
  /** Needed only by the file level, which is the one thing here that reads a route of its own. */
  projectId: string;
  modules: MapModule[];
  imports: MapImport[];
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

export function MapCanvas({
  projectId,
  modules,
  imports,
  junction,
  standings,
}: MapCanvasProps) {
  const [open, setOpen] = useState<string | null>(null);
  const [openFile, setOpenFile] = useState<string | null>(null);
  /**
   * Whether the whole map surface has the window.
   *
   * Held here and not in the frame around the picture, because the rail and the
   * crumbs have to grow with it: an overlay of the drawing alone is a bigger
   * picture you cannot navigate.
   */
  const [full, setFull] = useState(false);
  const matrix = useMemo(() => buildCommunities(modules, imports), [modules, imports]);
  const inside = useMemo(
    () => (open === null ? null : buildCommunity(matrix.members.get(open) ?? [], imports)),
    [open, matrix, imports],
  );
  const claimed = useMemo(() => claimedFiles(junction), [junction]);

  if (matrix.order.length === 0) {
    return (
      <p className="text-sm text-text-faint">Nothing here imports anything else.</p>
    );
  }

  const members = open === null ? [] : (matrix.members.get(open) ?? []);
  const enter = (title: string) => {
    setOpen(title);
    setOpenFile(null);
  };

  return (
    <Shell
      matrix={matrix}
      open={open}
      openFile={openFile}
      full={full}
      onFull={setFull}
      onCommunity={enter}
      onFile={setOpenFile}
      onTop={() => {
        setOpen(null);
        setOpenFile(null);
      }}
    >
      {openFile !== null ? (
        <FileLevel
          projectId={projectId}
          path={openFile}
          full={full}
          onFull={setFull}
          claims={claimsFor(junction, openFile)}
          standings={standings}
        />
      ) : open !== null && inside !== null ? (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-text-faint">
            {members.length} file{members.length === 1 ? "" : "s"} · {inside.links.length} import
            {inside.links.length === 1 ? "" : "s"} between them
          </p>
          <Traffic matrix={matrix} title={open} onOpen={enter} />
          {inside.refused.length > 0 ? (
            <Around
              members={members}
              imports={imports}
              refused={inside.refused}
              full={full}
              onFull={setFull}
              onOpen={setOpenFile}
            />
          ) : (
            <Graph
              drawn={inside.drawn}
              title={`${open} · ${members.length} file${members.length === 1 ? "" : "s"}`}
              full={full}
              onFull={setFull}
              onOpen={setOpenFile}
            />
          )}
          {/*
            The files, with the one verdict this level carries. The rail beside
            it lists the same names as doors; this list exists for the sentence
            attached to them, which the rail has no room for and which is about
            the junction rather than about navigation.
          */}
          <ol className="flex list-none flex-col gap-0.5">
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
                    className="ml-2 text-xs text-text-faint"
                    title="No approved decision names this file. Derived from the junction, and not a verdict about the code."
                  >
                    nothing asked for it
                  </span>
                )}
              </li>
            ))}
          </ol>
        </div>
      ) : (
        <Matrix matrix={matrix} onOpen={enter} full={full} onFull={setFull} />
      )}
    </Shell>
  );
}

/**
 * Everything that is on screen whatever level is open: where you are, and
 * everywhere else you could be.
 *
 * **Because the map had exactly one door and it was a picture.** A community
 * was entered by finding its name rotated ninety degrees along the top of a
 * table wider than the column holding it, and left by a single `← whole
 * project`. Going from one community to another was three clicks through the
 * top, and the ones scrolled out of the matrix were, in practice, not there.
 * The owner put it against a generated page that does this properly and the
 * comparison is fair: *"a navegação no mapa tem de ser tão fácil"*.
 *
 * So the rail is permanent and lists every community with its size, the crumb
 * trail says where you are and every ancestor is a button, and both stay put
 * when the drawing changes. Nothing here is reachable only by luck.
 *
 * **Full screen belongs here and not to the picture.** An overlay holding the
 * drawing alone — which is what the first version did — takes away the rail and
 * the crumbs at the exact moment there is most room for them, so growing the
 * window costs you the ability to go anywhere. The whole surface grows.
 */
function Shell({
  matrix,
  open,
  openFile,
  full,
  onFull,
  onCommunity,
  onFile,
  onTop,
  children,
}: {
  matrix: ReturnType<typeof buildCommunities>;
  open: string | null;
  openFile: string | null;
  full: boolean;
  onFull: (full: boolean) => void;
  onCommunity: (title: string) => void;
  onFile: (path: string) => void;
  onTop: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    if (!full) return;
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onFull(false);
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [full, onFull]);

  return (
    <div
      className={
        full
          ? "fixed inset-0 z-50 flex flex-col gap-3 overflow-auto bg-bg p-4"
          : "flex flex-col gap-4"
      }
    >
      <Crumbs community={open} file={openFile} onTop={onTop} onCommunity={onCommunity} />
      <div className="flex min-h-0 flex-1 items-start gap-4">
        <Rail
          matrix={matrix}
          open={open}
          openFile={openFile}
          full={full}
          onCommunity={onCommunity}
          onFile={onFile}
        />
        <div className="flex min-w-0 flex-1 flex-col gap-3">{children}</div>
      </div>
    </div>
  );
}

/** Where you are, and one button for every step back up. */
function Crumbs({
  community,
  file,
  onTop,
  onCommunity,
}: {
  community: string | null;
  file: string | null;
  onTop: () => void;
  onCommunity: (title: string) => void;
}) {
  const step = "rounded-sm px-1 text-text-muted hover:text-text";
  return (
    <nav aria-label="Where you are" className="flex flex-wrap items-center gap-1 text-xs">
      {community === null && file === null ? (
        <span className="text-text">whole project</span>
      ) : (
        <button type="button" onClick={onTop} className={step}>
          whole project
        </button>
      )}
      {community === null ? null : (
        <>
          <span className="text-text-faint">›</span>
          {file === null ? (
            <span className="font-mono text-text">{community}</span>
          ) : (
            <button
              type="button"
              onClick={() => onCommunity(community)}
              className={`${step} font-mono`}
            >
              {community}
            </button>
          )}
        </>
      )}
      {file === null ? null : (
        <>
          <span className="text-text-faint">›</span>
          <span className="font-mono text-text">{file}</span>
        </>
      )}
    </nav>
  );
}

/**
 * Every community, always, and the files of whichever one is open.
 *
 * Largest first: that is the order somebody looks in, and the count beside each
 * name makes the choice informed before the click rather than after it.
 *
 * **It does not change with the level.** A rail that emptied on the way down
 * would put a reader back where they started to reach a sibling — the exact
 * three-click journey this replaces. The current one is marked, so the list
 * doubles as *where am I* without a second reading of the crumbs.
 */
function Rail({
  matrix,
  open,
  openFile,
  full,
  onCommunity,
  onFile,
}: {
  matrix: ReturnType<typeof buildCommunities>;
  open: string | null;
  openFile: string | null;
  full: boolean;
  onCommunity: (title: string) => void;
  onFile: (path: string) => void;
}) {
  const listed = [...matrix.members].sort((a, b) => b[1].length - a[1].length);
  const inside = open === null ? [] : (matrix.members.get(open) ?? []);
  const row = "flex w-full items-baseline justify-between gap-2 rounded-sm px-2 py-0.5 text-left font-mono text-[11px]";
  return (
    <aside
      aria-label="Every community"
      className={
        (full ? "h-full " : "max-h-[560px] ") +
        "w-56 shrink-0 overflow-y-auto rounded-lg border border-border bg-surface-sunken p-2"
      }
    >
      {/*
        What is open comes first, and the whole list after it.

        The page this was measured against lists communities and then the open
        one's files, which works at its twenty. At sixty-four the files land
        sixty-four rows down a scrolling column — present, and no more reachable
        than they were in the matrix. So the order follows where you are.
      */}
      {open === null ? null : (
        <>
          <h3 className="px-2 pb-1 font-display text-xs font-medium uppercase tracking-wider text-text-faint">
            {open}
          </h3>
          <ul className="flex list-none flex-col">
            {inside.map((path) => (
              <li key={path}>
                <button
                  type="button"
                  aria-current={path === openFile ? "true" : undefined}
                  onClick={() => onFile(path)}
                  title={path}
                  className={
                    path === openFile
                      ? `${row} shrink-0 bg-surface-raised text-text`
                      : `${row} shrink-0 text-text-muted hover:text-text`
                  }
                >
                  <span className="truncate">{moduleName(path)}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      <h3
        className={
          "px-2 pb-1 font-display text-xs font-medium uppercase tracking-wider text-text-faint" +
          (open === null ? "" : " pt-3")
        }
      >
        Communities
      </h3>
      <ul className="flex list-none flex-col">
        {listed.map(([title, files]) => (
          <li key={title}>
            <button
              type="button"
              aria-current={title === open ? "true" : undefined}
              onClick={() => onCommunity(title)}
              className={
                title === open
                  ? `${row} shrink-0 bg-surface-raised text-text`
                  : `${row} shrink-0 text-text-muted hover:text-text`
              }
            >
              <span className="truncate">{title}</span>
              <span className="text-text-faint">{files.length}</span>
            </button>
          </li>
        ))}
      </ul>
    </aside>
  );
}

/**
 * What this community leans on, and what leans on it — each one a door.
 *
 * **Navigation along the structure, which the rail cannot do.** The rail answers
 * *what else is there*, in one flat order. This answers *what does this one
 * actually touch*, which is the question somebody standing inside a community
 * has, and it takes them to the neighbour for the reason it is a neighbour.
 *
 * The two directions are drawn apart and never summed. `council` using `job` and
 * `job` using `council` are opposite facts, and one number over the pair would
 * say two communities are connected while hiding which way the arrow points —
 * the same flattening the matrix refuses by putting one mark above the diagonal
 * and the other below.
 *
 * Silence is said out loud. A community nothing imports and that imports nothing
 * is a real and interesting answer, and an empty row where chips normally sit
 * reads as a surface that failed to load.
 */
function Traffic({
  matrix,
  title,
  onOpen,
}: {
  matrix: ReturnType<typeof buildCommunities>;
  title: string;
  onOpen: (title: string) => void;
}) {
  const { uses, usedBy } = trafficFor(matrix, title);
  const chip =
    "rounded-pill border border-border px-2 py-0.5 font-mono text-[11px] text-text-muted hover:border-border-strong hover:text-text";
  const row = (label: string, traffic: ReturnType<typeof trafficFor>["uses"]) => (
    <div className="flex flex-wrap items-baseline gap-1">
      <span className="w-16 text-xs uppercase tracking-wide text-text-faint">{label}</span>
      {traffic.length === 0 ? (
        <span className="text-xs text-text-faint">nothing</span>
      ) : (
        traffic.map((one) => (
          <button key={one.title} type="button" onClick={() => onOpen(one.title)} className={chip}>
            {one.title} <span className="text-text-faint">{one.weight}</span>
          </button>
        ))
      )}
    </div>
  );
  return (
    <div className="flex flex-col gap-1">
      {row("uses", uses)}
      {row("used by", usedBy)}
    </div>
  );
}

/**
 * When the whole community will not draw: one file, and everything that touches it.
 *
 * **A refusal with a way forward, which is what it was missing.** The reasons are
 * still printed first and unchanged — the numbers that decided it, because a
 * drawing nobody can follow is worse than a sentence saying why. What follows
 * them now is the question a reader actually has next: *then show me one part of
 * it*. A neighbourhood is small by construction, so it draws where the whole
 * does not, and every link in it is a link that exists.
 *
 * It opens on the file the most of the community touches. That is the one whose
 * neighbourhood explains the most of why the whole refused, and picking
 * alphabetically would open on whatever happens to sort first.
 *
 * A slice can refuse too, and says so with the same words. A clique's
 * neighbourhood is the clique; promising a picture here and drawing an
 * unreadable one would be the failure this map exists to refuse, one level down.
 */
function Around({
  members,
  imports,
  refused,
  full,
  onFull,
  onOpen,
}: {
  members: string[];
  imports: MapImport[];
  refused: string[];
  full: boolean;
  onFull: (full: boolean) => void;
  onOpen: (path: string) => void;
}) {
  const counted = useMemo(
    () =>
      members
        .map((path) => ({ path, near: neighbourCount(members, imports, path) }))
        .sort((a, b) => b.near - a.near || a.path.localeCompare(b.path)),
    [members, imports],
  );
  const [centre, setCentre] = useState<string | null>(null);
  const at = centre !== null && members.includes(centre) ? centre : counted[0]?.path;
  const slice = useMemo(
    () => (at === undefined ? null : sliceAround(members, imports, at)),
    [members, imports, at],
  );

  return (
    <div className="flex flex-col gap-2">
      <Refused reasons={refused} />
      <p className="max-w-prose text-xs text-text-muted">
        One file at a time, then — this shows the chosen one and everything in this community that
        touches it, either way. It is a smaller picture and not a looser one.
      </p>
      {/*
        **The chosen one is marked in `--text`, and was marked in the brand colour.** The This-One
        Rule settles that a selection is `.ui-current` — a 2px inset rule on the leading edge, in
        `--text` — and the class does not fit a pill: an inset shadow is clipped to the border box,
        so on a 999px radius it draws a sliver of an arc rather than a rule. What carries is the
        rule's reasoning rather than its geometry — the ink and not the hue, and a mark that does
        not depend on a fill, because `--surface` and `--surface-raised` are the same white in the
        light theme and the rung this chip moves would vanish there on its own.
      */}
      <div role="group" aria-label="Around" className="flex flex-wrap gap-1">
        {counted.map((one) => (
          <button
            key={one.path}
            type="button"
            aria-pressed={one.path === at}
            title={one.path}
            onClick={() => setCentre(one.path)}
            className={
              one.path === at
                ? "rounded-pill border border-text bg-surface-raised px-2 py-0.5 font-mono text-[11px] text-text"
                : "rounded-pill border border-border px-2 py-0.5 font-mono text-[11px] text-text-muted hover:border-border-strong hover:text-text"
            }
          >
            {moduleName(one.path)} <span className="text-text-faint">{one.near}</span>
          </button>
        ))}
      </div>
      {slice === null ? null : slice.refused.length > 0 ? (
        <Refused reasons={slice.refused} />
      ) : (
        <Graph
          drawn={slice.drawn}
          title={`around ${moduleName(at!)} · ${slice.members.length - 1} neighbour${slice.members.length === 2 ? "" : "s"}`}
          full={full}
          onFull={onFull}
          onOpen={onOpen}
        />
      )}
    </div>
  );
}

/**
 * The same honesty one step quieter: the picture IS drawn, and part of it has
 * stopped arriving.
 *
 * Separate from {@link Refused} in tone as well as in words, and the difference
 * is the whole point. A refusal is final — that drawing is not coming. This is a
 * condition of the size it is being looked at at, the reader holds the controls
 * that change it, and the sentence is only useful if it says which one to reach
 * for. So it names the reading that survived, the one that did not, and the two
 * ways out, and it does not borrow the refusal's red-flag framing for something
 * a press of `+` undoes.
 */
function TooSmall({ reasons }: { reasons: string[] }) {
  if (reasons.length === 0) return null;
  return (
    <p className="max-w-prose text-xs text-text-muted">
      <span className="text-text">At this size only the shape reads</span> — which side of the
      diagonal the marks fall on, and how much is below it. The detail does not:{" "}
      {reasons.join("; ")}. Stand closer, go full screen, or open a community to read it.
    </p>
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
  full,
  onFull,
  claims,
  standings,
}: {
  projectId: string;
  path: string;
  full: boolean;
  onFull: (full: boolean) => void;
  claims: Anchored[];
  standings: Record<string, Standing>;
}) {
  const found = useFileItems(projectId, path);

  return (
    <div className="flex flex-col gap-2">
      {found.isError ? (
        <p className="text-sm text-text-faint">
          The núcleo could not read this file — it may have moved since the map was walked.
        </p>
      ) : found.data === undefined ? (
        <p className="text-sm text-text-faint">Reading the file…</p>
      ) : (
        <FileDrawing found={found.data} full={full} onFull={onFull} />
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
      <SectionTitle>Decisions that claim this file</SectionTitle>
      <ul className="flex flex-col gap-1">
        {claims.map((claim) => {
          const standing = standings[String(claim.decision_id)];
          return (
            <li key={claim.decision_id} className="max-w-prose text-xs text-text-muted">
              <span className="font-mono text-[11px] text-text-faint">
                {claim.spec_slug} §{claim.section}
              </span>{" "}
              {/*
                A rung of the neutral ladder, and it was the brand colour. `isSettled` calls this
                "the green/not-green split", which names the drift twice over: what shipped was
                neither green nor a state tone but `--accent`, the one colour reserved for the
                wordmark, links and the focus ring. "Stamped" is not one of the seven tones either,
                so it does not get a tone invented for it — it gets words, which `standingLabel`
                already supplies ("stamped", "lapsed — the code moved since", "nobody has looked").
                What is left to carry is emphasis, and emphasis in this system is one rung: the
                owner's word in `--text`, its absence in `--text-faint`.
              */}
              <span className={isSettled(standing) ? "text-text" : "text-text-faint"}>
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
function FileDrawing({
  found,
  full,
  onFull,
}: {
  found: FileItems;
  full: boolean;
  onFull: (full: boolean) => void;
}) {
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
          title={`${facts.items} declaration${facts.items === 1 ? "" : "s"}`}
          full={full}
          onFull={onFull}
          /*
            Three rungs of the neutral ladder, and two of the three were broken. Reachable from
            outside wore the brand colour — marking an identifier, which the Reserved Cyan Rule
            names outright — and what nothing reaches wore a `border-subtle` stroke, a colour this
            app has never had: `tailwind.css` declares `--color-border` and
            `--color-border-strong` and no third, so the utility compiled to nothing, SVG's default
            `stroke` is `none`, and those boxes shipped with no outline at all.

            So: exported takes `--text`, the same ink the This-One Rule marks a chosen thing with
            and the top of the ladder; ordinary keeps `--border`; and what nothing reaches keeps the
            sunken fill, which is the rung that was already carrying it and the one that reads as a
            recess rather than as a box.
          */
          accent={(id) => {
            const item = byId.get(id);
            if (item === undefined) return "fill-surface stroke-border";
            if (!item.exported && !reached.has(item.id)) {
              return "fill-surface-sunken stroke-border";
            }
            return item.exported ? "fill-surface stroke-text" : "fill-surface stroke-border";
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
            {/* `--text` and not `--accent`, for the same reason the box outline above is: this is
                an identifier's property, and the brand colour has three uses and this is not one.
                A rung up from the row's `--text-muted` is what "reachable from outside" needs. */}
            {item.exported ? <span className="text-text"> ·pub</span> : null}
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
  full,
  onFull,
}: {
  matrix: ReturnType<typeof buildCommunities>;
  onOpen: (title: string) => void;
  full: boolean;
  onFull: (full: boolean) => void;
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
        dependency that goes down. A heavier mark below it points backwards</span> — and no reordering removes it.
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
      <Stage
        full={full}
        onFull={onFull}
        title={`${matrix.order.length} communities, every one of them a drawing of its own`}
        natural={matrixWidth(
          matrix.order.length,
          matrix.order.reduce((longest, title) => Math.max(longest, title.length), 0),
        )}
        note={(zoom) => <TooSmall reasons={unreadableAt(zoom)} />}
      >
        <table className="m-3 border-collapse font-mono text-xs">
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
                  /*
                    The two marks this whole matrix exists to be counted by, and neither used to be
                    drawable: forwards wore the brand colour hand-diluted, and backwards a bare
                    `danger` name this app has never declared, so the utility compiled to nothing
                    and every dependency pointing backwards shipped as an empty cell. Both are
                    fills of `--text` at two weights — never a state tone, because red means
                    destroyed and a back edge is coupling, not damage. That rule stands; what
                    follows is what it cost to keep it.

                    THE DIAGONAL IS THE RULE, and the weights are the second reading rather than
                    the only one. Two weights of one hue cannot carry this distinction on their
                    own: `/15` against `/35` measured 1.93:1 in dark and 1.63:1 in light, against
                    the 3:1 a graphical object needs to be told apart, and no pair of opacities on
                    one hue reaches 3:1 in the light theme at all — `/15` against `/55` is the best
                    available and stops at 2.95:1. The way out was not a second hue, which would
                    have bought the number and spent the rule above. It was to stop asking colour
                    to do it alone: a solid, unbroken staircase down the diagonal turns "above or
                    below" into a question about POSITION against a visible boundary, which needs
                    no contrast between the two fills, survives both themes, and survives a reader
                    who cannot separate them by tone at all. The weights then only have to rank
                    two things the eye has already placed, and at `/15` against `/55` they measure
                    3.51:1 in dark and 2.95:1 in light.

                    Full strength and not `surface-sunken`, which is what the diagonal wore while
                    it was scenery: at the 50% this opens at, a near-background staircase was the
                    faintest thing on a picture whose whole geometry hangs off it.
                  */
                  const tone =
                    i === j
                      ? "bg-text"
                      : weight === undefined
                        ? ""
                        : j > i
                          ? "bg-text/15"
                          : "bg-text/55";
                  return (
                    <td
                      key={column}
                      title={weight === undefined ? undefined : `${row} uses ${column} — ${weight}`}
                      className={`h-[19px] w-[19px] border border-border text-center ${tone}`}
                    >
                      {weight ?? ""}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </Stage>
    </div>
  );
}

/**
 * The frame every drawing on this page is looked at through.
 *
 * **Because the size of these pictures is the project's decision and not the
 * design's.** A matrix of 64 communities is 1,350px wide and a layered
 * community can be wider; drawn at their own size in a 1,040px column they
 * overflow both ways, and a reader gets a window onto a corner. Every other
 * surface in this app can be made to fit by choosing what to show. This one
 * cannot: leaving a community out is the falsehood the whole map is built
 * against. So the drawing stays whole and the reader is given the two controls
 * that make a whole thing viewable — how far out to stand, and how much room to
 * stand in.
 *
 * **It opens at the fit rather than at 100%.** The first thing anybody wants
 * from a map is its shape, and a picture that opens scrolled into its top-left
 * corner hides exactly that. `fit` is arithmetic over the drawing's own width,
 * so it is decided before a pixel is painted and never flashes full size first.
 *
 * **Zoom and not a transform.** `transform: scale()` leaves the scroll box
 * measuring the unscaled child, so shrinking a drawing to a third would keep
 * the scrollbars of the full-size one — the picture fits and still cannot be
 * reached. `zoom` reflows, so the box sees what is actually drawn.
 *
 * Full screen is this app's own overlay and not the browser's Fullscreen API:
 * the API is a promise that can be refused, needs a gesture, and takes the
 * window away from the rest of the shell. This is a panel that grows, closes on
 * Escape, and cannot end up in a state the app did not put it in.
 */
function Stage({
  title,
  natural,
  full,
  onFull,
  note,
  children,
}: {
  /** What is being looked at, said inside the frame so full screen still says it. */
  title: ReactNode;
  /**
   * What this drawing stops saying at the zoom it is actually being shown at.
   *
   * Takes the zoom because the answer changes with it, and the zoom lives here:
   * the stage owns `chosen`, re-fits itself against its own box, and is the only
   * thing that knows how big the picture ended up. A caller computing this from
   * `fit` would be describing the drawing nobody is looking at the moment the
   * reader presses `+`.
   */
  note?: (zoom: number) => ReactNode;
  /** The drawing's own width in pixels, before anything shrinks it. */
  natural: number;
  /** Whether the whole map surface has the window. Owned by `Shell`, not here. */
  full: boolean;
  onFull: (full: boolean) => void;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [room, setRoom] = useState(ASSUMED_ROOM);
  /** `null` while the reader has not chosen — which is what lets `fit` follow the box. */
  const [chosen, setChosen] = useState<number | null>(null);

  useEffect(() => {
    const el = box.current;
    if (el === null || typeof ResizeObserver === "undefined") return;
    const watch = new ResizeObserver(() => {
      // Zero is what a box that has not been laid out reports, and feeding it to
      // `fitZoom` would answer with the smallest step for every drawing there is.
      if (el.clientWidth > 0) setRoom(el.clientWidth);
    });
    watch.observe(el);
    return () => watch.disconnect();
  }, [full]);

  const fit = fitZoom(natural, room);
  const at = chosen ?? fit;
  const step = "rounded-pill border border-border px-2 py-0.5 text-xs text-text-muted enabled:hover:text-text disabled:opacity-40";

  return (
    <div className={full ? "flex min-h-0 flex-1 flex-col gap-2" : "flex flex-col gap-2"}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-text-muted">{title}</span>
        <span className="ml-auto flex items-center gap-1">
          <button
            type="button"
            aria-label="Further out"
            disabled={zoomBy(at, -1) === at}
            onClick={() => setChosen(zoomBy(at, -1))}
            className={step}
          >
            −
          </button>
          {/* The number is a readout and not a control: there is nothing to press
              here, and a button that does nothing is worse than a word. */}
          <span className="w-10 text-center font-mono text-xs text-text-muted">{zoomLabel(at)}</span>
          <button
            type="button"
            aria-label="Closer in"
            disabled={zoomBy(at, 1) === at}
            onClick={() => setChosen(zoomBy(at, 1))}
            className={step}
          >
            +
          </button>
          <button type="button" onClick={() => setChosen(null)} className={step}>
            fit
          </button>
          <button type="button" onClick={() => onFull(!full)} className={step}>
            {full ? "close" : "full screen"}
          </button>
        </span>
      </div>
      {note?.(at)}
      {/*
        A window of its own size, and the zoom happens inside it.

        `h-[560px]` and not `max-h`: with a maximum, standing further out made
        the content shorter and the window shrank with it, so pressing `−`
        appeared to shrink the map itself and the page jumped under whoever
        pressed it. The owner's rule, and it is the right one — *"o zoom é única
        e exclusivamente para o que está dentro dessa janela"*. Full screen is
        the one thing that resizes the window, because that is what it is for.
      */}
      <div
        ref={box}
        className={
          (full ? "min-h-0 flex-1 " : "h-[560px] ") +
          "w-full overflow-auto rounded-lg border border-border bg-surface"
        }
      >
        <div style={{ zoom: at }}>{children}</div>
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
  title,
  full,
  onFull,
  onOpen,
  accent,
}: {
  drawn: Layout;
  /** What this drawing is of — carried into the frame, which keeps it in full screen. */
  title: string;
  full: boolean;
  onFull: (full: boolean) => void;
  onOpen?: (id: string) => void;
  accent?: (id: string) => string;
}) {
  const at = new Map(drawn.nodes.map((node) => [node.id, { x: node.x, y: node.y }]));
  for (const [id, where] of Object.entries(drawn.bends)) at.set(id, where);
  const isBend = (id: string) => drawn.bends[id] !== undefined;

  return (
    <Stage title={title} natural={drawn.width + PAD * 2} full={full} onFull={onFull}>
      <svg
        // This is a picture; “Every community” is its text alternative, and its name lets a reader decide to skip it.
        role="img"
        aria-label={title}
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
                className={segment.reversed ? "stroke-border-strong" : "stroke-border"}
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
    </Stage>
  );
}

export { moduleName };
