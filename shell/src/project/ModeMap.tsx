// §spec mapa-do-projeto
import { useId, useMemo, useState, type CSSProperties } from "react";
import { useProjectMap, useProjectSpecs, type ProjectMap } from "../data/project-map";
import { useMapFlows } from "../data/map-flows";
import { drawableLinks } from "../canvas/map-graphs";
import { Atlas, NotRead } from "./Atlas";
import { buildAtlas } from "./atlas-model";
import { AtlasLayers } from "./AtlasLayers";
import { StampsPanel } from "./StampsPanel";
import { ExtractSpec } from "./ExtractSpec";
import { JunctionPanel } from "./JunctionPanel";
import { DecisionsWaiting } from "./DecisionsWaiting";
import { TriagePanel } from "./TriagePanel";
import { SequenceView } from "./SequenceView";
import { StatesView } from "./StatesView";
import { Button, ErrorNote, StaleNote, StatCard, relativeText } from "../ui";

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

/** The tabs of the map, in the order they read: the drawing, then the decision layer, then the rest. */
const VIEWS = [
  "architecture",
  "layers",
  "specs",
  "junction",
  "stamps",
  "triage",
  "sequence",
  "unread",
  "states",
] as const;
type MapView = (typeof VIEWS)[number];

const VIEW_LABEL: Record<MapView, string> = {
  architecture: "Architecture",
  layers: "Layers",
  specs: "Specs",
  junction: "Junction",
  stamps: "Stamps",
  triage: "Triage",
  sequence: "Sequence",
  unread: "Not read",
  states: "States",
};

export function ModeMap({ projectId }: ModeMapProps) {
  const map = useProjectMap(projectId);
  /*
    Read here for the number on one tab, and by `ExtractSpec` for the picker behind it. Two
    readers of one query and not two queries: React Query answers both out of the same cache entry,
    so the number on the tab and the list behind it cannot disagree.
  */
  const specs = useProjectSpecs(projectId);
  const flows = useMapFlows(projectId);
  const [view, setView] = useState<MapView>("architecture");
  const readAgain = () => void map.refetch();
  const atlas = useMemo(
    () =>
      map.data === undefined
        ? undefined
        : buildAtlas(map.data.modules, map.data.imports, map.data.unread),
    [map.data],
  );

  /*
    One tab at a time, and the drawing first. Stacked, the drawing, the decision layer and the
    unread pile competed down one long column; as tabs the drawing gets the whole screen and each
    other question gets a page of its own.
  */
  return (
    <div className="ws-page ws-map">
      <header className="ws-map-head">
        <Lead data={map.data} failed={map.isError} />
        {map.data === undefined ? null : (
          <Reading
            data={map.data}
            updatedAt={map.dataUpdatedAt}
            reading={map.isFetching}
            failed={map.isError}
            onRead={readAgain}
          />
        )}
      </header>

      <Views
        view={view}
        onView={setView}
        data={map.data}
        specs={specs.data?.length ?? null}
        flows={flows.data?.flows.length ?? null}
        unread={atlas?.unreadFiles ?? null}
      />

      {view === "sequence" ? (
        // Needs no map data, so it still works when the map could not be read.
        <SequenceView projectId={projectId} />
      ) : view === "specs" ? (
        /*
          Two panels behind one tab, because they are the two ends of one loop: choosing what gets
          read, and answering what came back. Neither reads the map, which is why this tab still
          works on a project whose folder has moved.
        */
        <div className="flex flex-col gap-6">
          {map.data !== undefined && map.data.junction.counts.decisions === 0 ? (
            // One quiet line: the empty state is not the news here, the spec to read is.
            <p className="m-0 text-sm text-text-muted">
              Nothing decided yet — only what you approve from a spec enters the map.
            </p>
          ) : null}
          <ExtractSpec projectId={projectId} />
          <DecisionsWaiting projectId={projectId} />
        </div>
      ) : view === "states" ? (
        <StatesView projectId={projectId} />
      ) : map.isError && map.data === undefined ? (
        <ErrorNote>
          The núcleo could not read this project&rsquo;s map — its folder may have moved.{" "}
          <Button variant="quiet" onClick={readAgain} disabled={map.isFetching}>
            {map.isFetching ? "reading…" : "read it again"}
          </Button>
        </ErrorNote>
      ) : map.data === undefined || atlas === undefined ? (
        <p className="text-sm text-text-faint">Reading the project&rsquo;s tree…</p>
      ) : view === "architecture" ? (
        <Atlas data={map.data} atlas={atlas} />
      ) : view === "layers" ? (
        <AtlasLayers projectId={projectId} data={map.data} />
      ) : view === "unread" ? (
        <NotRead atlas={atlas} />
      ) : (
        <div className="flex flex-col gap-4">
          <Decisions data={map.data} onOpen={setView} />
          <Derived projectId={projectId} view={view} data={map.data} />
        </div>
      )}

      {/*
        The absence is still said — a map that implies a layer it has not got is the false
        confidence this mode exists to cure — but once, quietly, and not under the canvas, which
        runs to the bottom of the window.
      */}
      {view === "architecture" || view === "layers" ? null : (
        <p className="ws-foot">
          Not on this map yet: evidence — nothing here reads a test run or a gate.
        </p>
      )}
    </div>
  );
}

/**
 * The one sentence the map leads with: how big the ground is, and how it holds together.
 */
function Lead({ data, failed }: { data: ProjectMap | undefined; failed: boolean }) {
  if (data === undefined) {
    return (
      <p className="font-display text-lg leading-snug text-text-muted">
        {failed ? "This map could not be read." : "Reading this project’s map…"}
      </p>
    );
  }
  const files = data.modules.length;
  const sides = new Set(data.modules.map((m) => `${m.path.split("/")[0]}\0${m.reader}`)).size;
  const routes = data.seam?.served.length ?? 0;
  const broken = data.seam?.unmatched.length ?? 0;
  const tone =
    files === 0
      ? "var(--text-muted)"
      : broken > 0
        ? "var(--tone-danger-fg)"
        : "var(--tone-active-fg)";

  return (
    <section
      aria-label="This project"
      className="ws-lead"
      style={{ "--ws-lead-tone": tone } as CSSProperties}
    >
      <p className="ws-lead-line">
        {files === 0
          ? "Nothing here is code this map can read."
          : `${files} files in ${sides} ${sides === 1 ? "side" : "sides"}${
              routes > 0 && sides > 1 ? `, joined by ${routes} routes` : ""
            }.`}
        {broken > 0 ? ` ${broken} ${broken === 1 ? "call reaches" : "calls reach"} nothing.` : ""}
      </p>
    </section>
  );
}

/**
 * §5.3's header as four figures: stamped, part-way, never looked at, on your desk.
 *
 * Four cards and not one sentence because they are four different claims — only the first is the
 * owner's green (§5.2), the third is debt and is meant to bother, the last is what asks for a hand
 * — and a line of `0 · 0 · 0 · 0` read them all at one weight. The numbers are the núcleo's own
 * tallies, never recounted here.
 *
 * With nothing approved there is nothing to count, and four noughts would read as a measurement
 * (§10, §11): the block says what it would take instead, and where to start.
 */
function Decisions({ data, onOpen }: { data: ProjectMap; onOpen: (view: MapView) => void }) {
  if (data.junction.counts.decisions === 0) {
    // One quiet line: the empty state is not the news here, the spec to read below it is.
    return (
      <p className="m-0 text-sm text-text-muted">
        Nothing decided yet — only what you approve from a spec enters the map.
      </p>
    );
  }
  const { stamps, triage_counts: triage } = data;
  return (
    <div className="ws-metrics">
      <StatCard
        label="Stamped"
        value={<span className="ws-verdict-settled">{stamps.settled}</span>}
        detail="Still as you wanted."
      />
      <StatCard
        label="Part-way"
        value={<span className="ws-verdict-partial">{stamps.partial}</span>}
        detail="Unfinished, and you said so."
      />
      <StatCard
        label="Never looked at"
        value={triage.unseen}
        detail="Debt, and meant to bother. History is never stamped."
      />
      <StatCard
        label="On your desk"
        value={
          triage.waiting > 0 ? (
            <button type="button" className="ws-lead-door" onClick={() => onOpen("stamps")}>
              {triage.waiting}
            </button>
          ) : (
            triage.waiting
          )
        }
        detail="Stamps that lapsed, and what the triager flagged."
      />
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
  flows,
  unread,
}: {
  view: MapView;
  onView: (view: MapView) => void;
  data: ProjectMap | undefined;
  /** How many documents there are to read, or `null` while that listing is unanswered. */
  specs: number | null;
  /** How many flows the project declares, or `null` while that answer is unread. */
  flows: number | null;
  /** Files the map could not follow, or `null` while the map is unread. */
  unread: number | null;
}) {
  const said = useId();
  const approved = data?.junction.counts.decisions ?? 0;
  /* Nothing has been read against the code yet, so nothing counted over it is a measurement. */
  const layered = data !== undefined && approved > 0;

  const files = data?.modules.length ?? null;
  const counts: Record<MapView, { of: number | null; means: string }> = {
    architecture: {
      of: files,
      means: "The folders, and what leans on what.",
    },
    layers: {
      of: data?.layers?.violations.length ?? null,
      means: "The modules stacked by layer, and the imports that climb.",
    },
    unread: {
      of: unread,
      means: "Source in languages the map cannot follow.",
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
    sequence: {
      of: flows,
      means:
        flows === null
          ? "Still reading the declared flows."
          : `${flows} declared flow${flows === 1 ? "" : "s"}, each checked against the code.`,
    },
    specs: {
      of: specs,
      means:
        specs === null
          ? "Still looking for this project's documents."
          : `${specs} document${specs === 1 ? "" : "s"} a model could read decisions out of.`,
    },
    states: {
      of: null,
      means: "Lifecycles the code's UPDATE … SET status statements draw.",
    },
  };

  /*
    A door with nothing behind it is not drawn: three chips reading "—" were most of the row on a
    project with no decision, and said the same absence three times. The one you are behind stays.
  */
  const doors = VIEWS.filter(
    (candidate) =>
      candidate === "architecture" ||
      candidate === "layers" ||
      candidate === "specs" ||
      candidate === "states" ||
      candidate === view ||
      (counts[candidate].of ?? 0) > 0 ||
      (candidate !== "unread" && counts[candidate].of !== null),
  );

  return (
    <nav aria-label="Views of this map" className="ui-switch ws-map-tabs">
      {doors.map((candidate) => {
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
            {of === null ? (
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
 * The line under the lead: what holds the files together, and when they were read.
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
  const { modules, imports } = data;
  // Counted rather than taken from `imports.length`: this is the number of links the map would
  // actually draw, which drops any edge with an end it cannot find.
  const links = drawableLinks(modules, imports);

  return (
    <div className="flex flex-col gap-1">
      {/*
        One line of prose, and set as prose: no flex gap between its pieces (which put a wide gap
        before each "·"), and the time as words rather than `RelativeTime`'s mono, which is the
        face for a value the daemon wrote. "just now" is a phrase this line says, not an id.
        The exact time stays on hover and in `dateTime`.
      */}
      <p className="m-0 text-xs text-text-muted">
        {/* The file count is the lead's; repeating it here is what made the two disagree once. */}
        <span className="tabular-nums">{links}</span> import{links === 1 ? "" : "s"} between them
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
}: {
  projectId: string;
  view: Exclude<MapView, "specs" | "architecture" | "layers" | "unread" | "states">;
  data: ProjectMap;
}) {
  const {
    modules,
    junction,
    standings,
    stamps,
    triage,
    triage_counts,
    git_would_not_answer,
    recency,
    last_triaged_at,
  } = data;

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
