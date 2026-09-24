// §spec mapa-do-projeto
import { useId, useMemo, useState } from "react";
import { useProjectMap, useProjectSpecs, type ProjectMap } from "../data/project-map";
import { Boundary } from "../canvas/Boundary";
import { MapCanvas } from "../canvas/MapCanvas";
import { declaredCoverage } from "../canvas/map-model";
import { buildCommunities, drawableLinks, type CommunityMatrix } from "../canvas/map-graphs";
import { StampsPanel } from "./StampsPanel";
import { ExtractSpec } from "./ExtractSpec";
import { JunctionPanel } from "./JunctionPanel";
import { DecisionsWaiting } from "./DecisionsWaiting";
import { TriagePanel } from "./TriagePanel";
import { Button, ErrorNote, StaleNote, relativeText } from "../ui";

/**
 * "What is in here, what did nobody ask for, and what did this project actually decide?"
 *
 * Two of the three layers are here, the join between them, and the two axes read over it: the
 * structure, derived off disk and always true; the intention — a model reading one spec and
 * proposing the decisions it fixes, which nothing enters the map without the owner answering line
 * by line; the junction, which is decision 3 of the spec and the reason the other two are worth
 * deriving; the stamps, which are what the owner said about each line and whether it is still true;
 * and the triage, which is what a model thought was worth their eyes. The evidence layer is not
 * here, and the mode says so in words rather than showing a map that looks complete and is not. A
 * surface that implies it has layers it does not have is the false confidence again, with better
 * pixels.
 *
 * **The junction, the stamps and the triage are three axes and never one reading.** §5 refuses to
 * flatten them — *"achatá-las numa só punha o triador e o dono a falar pela mesma boca"* — so they
 * are three panels with three headings, and no number on this screen has already decided how they
 * combine. §6.1 is the sharpest edge of that rule and it is kept by rendering rather than promised:
 * a silence is a claim about the triager, a green is the owner's, and nothing here draws the two in
 * a way that could be mistaken one for the other.
 *
 * **Five doors and not one column, which is that same argument one floor down.** Stacked, this
 * mode was 3557px: the picture ended at 1723 and everything under it was reached by scrolling past
 * what you came for, so the three axes §5 keeps apart were flattened into a scrollbar. The row
 * above them says how much is behind each — the idiom the inspector next door already uses for
 * exactly this.
 *
 * **What a door may not hide is the seam.** §16.5 is written against a descent that takes one with
 * it, and the header the picture used to carry is a caveat on all five: the junction, the stamps
 * and the triage are counted over the same partial reading the drawing is. So {@link Boundary}
 * sits above the row rather than inside the picture, on screen whichever door is open.
 *
 * **The panels read different questions, and one failing does not silence the others.** The
 * structure is derived by walking the project's folder; the specs are a listing of a folder; the
 * pile is a table the daemon answers without touching disk at all — `get_project_map_decisions`
 * resolves the row and never the folder, deliberately, "so a project whose folder has moved still
 * has a pile, which is exactly what somebody looking at a broken project wants". A map that could
 * not be read leaves the four doors that need it saying so, and leaves the fifth working.
 *
 * **The junction is not a fourth question, which is why it is not a fourth query.** It comes back
 * on the same answer as the structure — `GET /projects/{id}/map` returns both, flattened — so it
 * is drawn off the one query this mode holds. A query apiece would ask the daemon to walk the same
 * tree four times per open, and would put four answers to one question on one screen, free to
 * disagree about a project whose folder moved between them.
 */

export interface ModeMapProps {
  projectId: string;
}

/** The five doors, in the order they read. */
const VIEWS = ["picture", "junction", "stamps", "triage", "specs"] as const;
type MapView = (typeof VIEWS)[number];

const VIEW_LABEL: Record<MapView, string> = {
  picture: "Picture",
  junction: "Junction",
  stamps: "Stamps",
  triage: "Triage",
  specs: "Specs",
};

export function ModeMap({ projectId }: ModeMapProps) {
  const map = useProjectMap(projectId);
  /*
    Read here for the number on one door, and by `ExtractSpec` for the picker behind it. Two
    readers of one query and not two queries: React Query answers both out of the same cache entry,
    so the number on the door and the list behind it cannot disagree.
  */
  const specs = useProjectSpecs(projectId);
  const [view, setView] = useState<MapView>("picture");
  /*
    Built once, here, for two readers: the header's row counting what points backwards and the
    matrix that draws it below its diagonal. One build is one number — and the seriation is the
    dearest thing this mode computes, so it is not paid twice per open either.
  */
  const matrix = useMemo(
    () => (map.data === undefined ? null : buildCommunities(map.data.modules, map.data.imports)),
    [map.data],
  );
  const readAgain = () => void map.refetch();

  return (
    <div className="flex flex-col gap-6">
      {/*
        The verdict and the seam, above every door. Absent until there is an answer to draw them
        from — a header of dashes over a map still loading is a measurement nobody took.
      */}
      {map.data === undefined ? null : (
        <Boundary
          modules={map.data.modules}
          imports={map.data.imports}
          unread={map.data.unread}
          foreign={map.data.foreign}
          seam={map.data.seam}
          direction={matrix ?? undefined}
          onMatrix={() => setView("picture")}
          meta={
            <Reading
              data={map.data}
              updatedAt={map.dataUpdatedAt}
              reading={map.isFetching}
              failed={map.isError}
              onRead={readAgain}
            />
          }
        />
      )}

      <Views view={view} onView={setView} data={map.data} specs={specs.data?.length ?? null} />

      {view === "specs" ? (
        /*
          Two panels behind one door, because they are the two ends of one loop: choosing what gets
          read, and answering what came back. Neither reads the map, which is why this door still
          works on a project whose folder has moved.
        */
        <div className="flex flex-col gap-8">
          <ExtractSpec projectId={projectId} />
          <DecisionsWaiting projectId={projectId} />
        </div>
      ) : map.isError ? (
        <ErrorNote>
          The núcleo could not read this project&rsquo;s map — its folder may have moved.{" "}
          <Button variant="quiet" onClick={readAgain} disabled={map.isFetching}>
            {map.isFetching ? "reading…" : "read it again"}
          </Button>
        </ErrorNote>
      ) : map.data === undefined || matrix === null ? (
        <p className="text-sm text-text-faint">Reading the project&rsquo;s tree…</p>
      ) : (
        <Derived projectId={projectId} view={view} data={map.data} matrix={matrix} />
      )}

      {/*
        One line, and it was four sentences at the foot of every door: every visit ended on an
        absence. The absence is still said — a map that implies a layer it has not got is the
        false confidence this mode exists to cure — but once, and quietly.
      */}
      <p className="m-0 text-xs text-text-faint">
        Not on this map yet: evidence — nothing here reads a test or a gate.
      </p>
    </div>
  );
}

/**
 * The row of doors, and the number on each.
 *
 * **A zero is drawn only where a zero was measured.** `JunctionPanel` spends a paragraph refusing to draw
 * a grid of zeros on a project with no approved decision — *"a row of `0`s reads as a measurement,
 * and here nothing has been measured"* — and a chip reading `Junction 0` is that same claim in
 * less space and with more authority. So until one decision is approved, those three doors carry
 * an em dash: this app's own mark for a reading nobody took, with the reason on hover, read out as
 * the button's description, and the whole sentence behind the door.
 *
 * The specs door is not like them. A count of documents on disk is a real measurement whatever
 * else this project has or has not got, so it says the number even when the number is nought.
 *
 * **The shared switch track, and the door you are behind is marked three ways at once**: the
 * raised rung, the top ink, and `.ui-current`'s 2px rule on the leading edge. It was the rule
 * alone, inside a chip that already had a border, and "Picture" and "Junction" could barely be
 * told apart; a rung alone vanishes in the light theme, where `--surface` and `--surface-raised`
 * are the same white. The rule survives both themes and the ink carries the rest.
 */
function Views({
  view,
  onView,
  data,
  specs,
}: {
  view: MapView;
  onView: (view: MapView) => void;
  data: ProjectMap | undefined;
  /** How many documents there are to read, or `null` while that listing is unanswered. */
  specs: number | null;
}) {
  const said = useId();
  const approved = data?.junction.counts.decisions ?? 0;
  /* Nothing has been read against the code yet, so nothing counted over it is a measurement. */
  const layered = data !== undefined && approved > 0;

  const counts: Record<MapView, { of: number | null; means: string }> = {
    picture: {
      of: null,
      means: "The project as it is on disk, which is the one layer that is always true.",
    },
    junction: {
      of: layered ? approved : null,
      means: layered
        ? `${approved} approved decision${approved === 1 ? "" : "s"} to read the structure against.`
        : "No decision has been approved yet, so there is nothing to read the structure against.",
    },
    stamps: {
      of: layered ? data.triage_counts.waiting : null,
      means: layered
        ? "Decisions on your desk: the stamps that lapsed, plus what the triager flagged."
        : "Nothing has been approved yet, so there is nothing to stamp.",
    },
    triage: {
      of: layered ? data.triage_counts.flagged : null,
      means: layered
        ? "Decisions a model looked at and asked for your eyes on."
        : "Nothing has been approved yet, so the triager has nothing to look at.",
    },
    specs: {
      of: specs,
      means:
        specs === null
          ? "Still looking for this project's documents."
          : `${specs} document${specs === 1 ? "" : "s"} a model could read decisions out of.`,
    },
  };

  return (
    <nav aria-label="Views of this map" className="ui-switch">
      {VIEWS.map((candidate) => {
        const { of, means } = counts[candidate];
        const current = candidate === view;
        return (
          <button
            key={candidate}
            type="button"
            onClick={() => onView(candidate)}
            aria-current={current ? "true" : undefined}
            aria-describedby={`${said}-${candidate}`}
            title={means}
            className={
              current
                ? "ui-switch-seg ui-current inline-flex items-baseline gap-2 bg-surface-raised! text-text!"
                : "ui-switch-seg inline-flex items-baseline gap-2"
            }
          >
            {VIEW_LABEL[candidate]}
            {candidate === "picture" ? null : of === null ? (
              <>
                <span aria-hidden="true" className="font-mono text-text-faint">
                  —
                </span>
                <span className="sr-only">not measured</span>
              </>
            ) : (
              <span className="font-mono text-text-faint">{of}</span>
            )}
            <span id={`${said}-${candidate}`} hidden>
              {means}
            </span>
          </button>
        );
      })}
    </nav>
  );
}

/**
 * The line under the verdict: the size of what this reader could read, and when it read it.
 *
 * **The size used to be the page's hero** — `245` at display size, the largest thing on the
 * screen — and it answers nothing: a project's size is not whether it is fine. It is metadata,
 * and it sits where metadata sits.
 *
 * **When the tree was read is the half that was missing.** The map is read on open and never
 * polled (walking a thousand files every few seconds is the wrong trade), so a map left open over
 * lunch is a lunch-old map with nothing saying so. Principle 4 of this product is *never claim
 * currency you do not have*: the time is printed, and reading it again is one press.
 *
 * It reports what this half alone knows. It once reported `modules.filter(m => !m.declares)`
 * under the words "declaring nothing they implement", which is the junction's `unclaimed` by
 * another derivation — and the two disagreed by four on one screen. The junction owns that count.
 */
function Reading({
  data,
  updatedAt,
  reading,
  failed,
  onRead,
}: {
  data: ProjectMap;
  /** react-query's `dataUpdatedAt`: when the tree was last read successfully. */
  updatedAt: number;
  reading: boolean;
  failed: boolean;
  onRead: () => void;
}) {
  const { modules, imports, foreign } = data;
  // Counted rather than taken from `imports.length`: this is the number of links the map would
  // actually draw, which drops any edge with an end it cannot find.
  const links = drawableLinks(modules, imports);
  // §8 on screen: a bare `§7` names a section of *some* document, and the header says which.
  // Counted over the files that name a section and not over `modules.length` — a file with no `§`
  // has nothing to declare.
  const declared = declaredCoverage(modules, foreign);

  return (
    <div className="flex flex-col gap-1">
      {/*
        One line of prose, and set as prose: no flex gap between its pieces (which put a wide gap
        before each "·"), and the time as words rather than `RelativeTime`'s mono, which is the
        face for a value the daemon wrote. "just now" is a phrase this line says, not an id.
        The exact time stays on hover and in `dateTime`.
      */}
      <p className="m-0 text-xs text-text-muted">
        <span className="font-mono tabular-nums text-text">{modules.length}</span> module
        {modules.length === 1 ? "" : "s"} this reader could read · {links} link
        {links === 1 ? "" : "s"}
        {declared.citing > 0
          ? ` · ${declared.saying} of ${declared.citing} files naming a section say which document`
          : ""}
        {updatedAt > 0 ? (
          <>
            {" · read "}
            <time
              dateTime={new Date(updatedAt).toISOString()}
              title={new Date(updatedAt).toLocaleString()}
            >
              {relativeText(updatedAt, Date.now())}
            </time>
          </>
        ) : null}
        {" · "}
        <Button variant="quiet" onClick={onRead} disabled={reading}>
          {reading ? "reading…" : "read again"}
        </Button>
      </p>
      {failed ? <StaleNote dataUpdatedAt={updatedAt} /> : null}
    </div>
  );
}

/**
 * Everything the one map query answers, drawn one door at a time.
 *
 * Named for the query and not for a layer, because it draws four different things and a component
 * called `Structure` that also rendered the junction would be lying about itself in the file that
 * argues hardest against exactly that.
 *
 * Each of the four reads this one answer rather than fetching its own, which is the rule the
 * mode's header states: a query apiece would walk a thousand-file tree four times per open, and
 * would put four readings of one question on one screen.
 */
function Derived({
  projectId,
  view,
  data,
  matrix,
}: {
  projectId: string;
  view: MapView;
  data: ProjectMap;
  matrix: CommunityMatrix;
}) {
  const {
    modules,
    imports,
    junction,
    standings,
    stamps,
    triage,
    triage_counts,
    git_would_not_answer,
    recency,
    last_triaged_at,
  } = data;

  if (view === "picture") {
    /*
      The structure layer as a picture: the whole project, one community, and one file's own
      declarations. The file level is the one thing here that reads a route of its own — it reads
      one file, which this answer does not carry and could not carry cheaply, and that route is
      asked only once a reader opens something, so the cost lands on the click.
    */
    return (
      <MapCanvas
        projectId={projectId}
        modules={modules}
        imports={imports}
        junction={junction}
        standings={standings}
        matrix={matrix}
      />
    );
  }

  if (view === "junction") {
    // The modules ride along so the panel can check its own "nobody asked for" pile against the
    // citations the sides above are counted from: two derivations of one fact, on one screen.
    return <JunctionPanel junction={junction} projectId={projectId} modules={modules} />;
  }

  if (view === "stamps") {
    /*
      The standings, §5.3's header and `git_would_not_answer` all come back on this same answer,
      flattened, which is why this panel has no query of its own.
    */
    return (
      <StampsPanel
        projectId={projectId}
        junction={junction}
        standings={standings}
        stamps={stamps}
        triage={triage}
        triageCounts={triage_counts}
        gitWouldNotAnswer={git_would_not_answer}
      />
    );
  }

  /*
    The third axis. It holds one query the map cannot serve: §6.2's silenced pile has a door of its
    own, because *sempre acessível* has to survive this map failing to read a folder.
  */
  return (
    <TriagePanel
      projectId={projectId}
      junction={junction}
      triage={triage}
      counts={triage_counts}
      recency={recency}
      lastTriagedAt={last_triaged_at}
    />
  );
}
