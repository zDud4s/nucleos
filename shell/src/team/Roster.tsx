import { useMemo } from "react";
import { Link } from "@tanstack/react-router";
import { useAgents } from "../data/agents";
import {
  teamRunIsAlive,
  useLiveTeamRuns,
  useTeams,
  type TeamRun,
  type TeamView,
} from "../data/teams";
import { Quiet } from "../ui";
import {
  ROW_GAP,
  buildRoster,
  clip,
  placeRoster,
  type RosterBox,
  type RosterNode,
} from "./roster-graph";

/**
 * `Roster` — the department drawn as its own org chart.
 *
 * The department on top with the limits it runs under, its director below that, its roster below
 * that, and — only when there is any — the work each specialist is holding right now. Read-only:
 * nothing here writes, and the only thing it can be clicked into is the task a piece of work
 * belongs to.
 *
 * **The structure is drawn whether or not anything is running.** The first version made live work
 * the third and last rank, so an idle department drew three boxes and the sentence "nothing in
 * flight" — a picture of the absence of work rather than of how the department is put together.
 * The standing facts now have ranks of their own; work is the rank that comes and goes.
 *
 * **Why a graph when `RosterMatrix.tsx` decided a roster is a matrix.** That decision was about
 * the whole house — a membership across nine specialists and six departments, which reads better
 * as a grid, and the console still draws it that way. One department is a containment: it holds a
 * director, who holds a roster, whose members hold work. Containment is a tree.
 *
 * **SVG by hand, and `roster-graph.ts` for everything that is not drawing** — the same division
 * `JobProgressGraph.tsx` makes, and for the same reason: the interesting half is arithmetic, and
 * arithmetic tested through a renderer is arithmetic tested badly.
 */

export interface RosterProps {
  team: TeamView;
  /** Already filtered to this department by the bench. */
  runs: TeamRun[];
}

export function Roster({ team, runs }: RosterProps) {
  /*
    Only the live ones, and only their ids. A finished run is history — the Work tab owns that —
    and the daemon caps the live ones at four (`core/src/team_trigger.rs:48`), so this fan-out is
    bounded by the núcleo rather than by anything decided here.
  */
  const live = runs.filter((row) => teamRunIsAlive(row.state)).map((row) => row.id);
  const flight = useLiveTeamRuns(live);
  const agents = useAgents();
  const teams = useTeams();

  const catalogue = agents.data;
  const model = useMemo(
    () =>
      buildRoster({
        team,
        agents: catalogue ?? [],
        teams: teams.data ?? [],
        runs: flight.runs,
      }),
    [team, catalogue, teams.data, flight.runs],
  );

  /*
    The layout depends on ids, labels and parents — never on a state. States change on every poll,
    and a chart that relaid itself each time would move under the cursor of whoever is reading it
    while nothing structural had changed. `JobProgressGraph.tsx:26-35` memoises on the same fact.
  */
  const shape = useMemo(
    () => JSON.stringify(model.nodes.map((node) => [node.id, node.label, node.parent])),
    [model.nodes],
  );
  // `shape` is the real dependency: same ids, same labels, same parents, same picture.

  const view = useMemo(() => placeRoster(model.nodes), [shape]);

  const by = new Map(model.nodes.map((node) => [node.id, node]));
  const at = new Map(view.boxes.map((box) => [box.id, box]));

  if (catalogue === undefined) {
    return <Quiet says="reading the catalogue…" />;
  }

  const headless = !model.nodes.some((node) => node.layer === 1);
  const alone = !model.nodes.some((node) => node.layer === 2);
  const inFlight = model.nodes.some((node) => node.layer === 3);
  const waiting = live.length > 0 && flight.pending && flight.runs.length === 0;

  return (
    <div className="teams-org">
      <div className="teams-org-scroll">
        {/*
          `role="group"` and not the `role="img"` the fleet's graph carries. An image's children are
          presentational, and the work boxes are links — calling this a picture would take the work
          out of the accessibility tree along with the way into it.
        */}
        <svg
          className="teams-org-svg"
          role="group"
          aria-label={`${team.name} — how this team is put together`}
          viewBox={`0 0 ${view.width} ${view.height}`}
          width={view.width}
          height={view.height}
        >
          <g className="teams-org-edges">
            {view.edges.map((edge) => {
              const from = at.get(edge.from);
              const to = at.get(edge.to);
              if (from === undefined || to === undefined) return null;
              const x1 = from.x + from.width / 2;
              const y1 = from.y + from.height;
              const x2 = to.x + to.width / 2;
              const y2 = to.y;
              /*
                A crossing edge is routed and not curved, and this is the one thing in the drawing
                that was decided by looking at it rather than by reasoning. A bezier from the
                director to a box two ranks down passes THROUGH the roster rank — it went under
                the `reviewer` box, which paints over it, and came out of its right edge. The
                picture then said the director's own work belonged to `reviewer`, which is the
                exact fact this edge exists to deny. So it drops into the gap above the roster,
                runs across it, and comes down in the crossing item's own column — which has no box
                in that rank, by construction.
              */
              const d = edge.crosses
                ? `M ${x1} ${y1} V ${y1 + ROW_GAP / 2} H ${x2} V ${y2}`
                : `M ${x1} ${y1} C ${x1} ${y1 + 22}, ${x2} ${y2 - 22}, ${x2} ${y2}`;
              return (
                <path
                  key={`${edge.from}->${edge.to}`}
                  className={edge.crosses ? "teams-org-edge teams-org-edge-far" : "teams-org-edge"}
                  d={d}
                />
              );
            })}
          </g>
          <g>
            {view.boxes.map((box) => {
              const node = by.get(box.id);
              if (node === undefined) return null;
              return <Box key={box.id} node={node} box={box} />;
            })}
          </g>
        </svg>
      </div>

      {/*
        One sentence, not three. A department nobody has staffed is missing a director, a roster
        AND any work, and saying all three stacks up as noise around a box that already shows an
        empty department. The most upstream fact is the one that explains the others, and it is
        the only one worth printing.
      */}
      {headless ? (
        <Quiet
          says="nobody is in charge of this team yet — it will refuse every task until somebody is."
        />
      ) : alone ? (
        <Quiet says="nobody on the roster yet" />
      ) : waiting ? (
        <Quiet says="reading the work…" />
      ) : (
        !inFlight && <Quiet says="nothing in flight" />
      )}
    </div>
  );
}

/**
 * One box, and the one fact it adds to its own name.
 *
 * A department is how many people are on it; the one at the top directs; a specialist is what it
 * does, or that the catalogue no longer has it; a piece of work is its state. Those are four
 * different sentences, and each is the one thing somebody would ask about that box — which is why
 * this is not a single field.
 */
function spoken(node: RosterNode): string {
  return node.layer === 1 || node.layer === 3 ? node.state : node.said;
}

/** The rank a box belongs to, as a class. Only work carries a state tone. */
const KIND: Record<number, string> = {
  0: "teams-org-dept",
  1: "teams-org-lead",
  2: "teams-org-who",
  3: "teams-org-item",
};

function Box({ node, box }: { node: RosterNode; box: RosterBox }) {
  const tone = node.layer === 3 ? ` teams-org-${node.state}` : "";
  const gone = node.missing ? " teams-org-missing" : "";

  /* One formula for four ranks: the name first, then its one line, then whatever else it carries.
     A box that is two lines of name deep pushes the rest down rather than writing over it. */
  const said = 20 + (box.lines.length - 1) * 13 + 16;

  const body = (
    <>
      <rect className="teams-org-box" width={box.width} height={box.height} rx={8} />
      {box.lines.map((line, index) => (
        <text
          className="teams-org-name"
          key={line + index}
          x={box.width / 2}
          y={20 + index * 13}
          textAnchor="middle"
        >
          {line}
        </text>
      ))}
      <text className="teams-org-said" x={box.width / 2} y={said} textAnchor="middle">
        {clip(node.said)}
      </text>
      {node.facts.map((fact, index) => (
        <text
          className="teams-org-fact"
          key={fact}
          x={box.width / 2}
          y={said + 15 + index * 14}
          textAnchor="middle"
        >
          {fact}
        </text>
      ))}
      {node.layer === 3 && (
        <text
          className="teams-org-state"
          x={box.width / 2}
          y={box.height - 9}
          textAnchor="middle"
        >
          {/* The glyph is decoration; the word beside it is the answer — for anything that does
              not render a glyph, and for anyone who cannot tell the two tones apart. */}
          <tspan className="teams-org-mark" aria-hidden="true">
            {MARK[node.state] ?? "·"}
          </tspan>
          <tspan dx="5">{node.state}</tspan>
        </text>
      )}
    </>
  );

  return (
    <g
      className={`teams-org-node ${KIND[node.layer]}${tone}${gone}`}
      transform={`translate(${box.x}, ${box.y})`}
      aria-label={`${node.label} — ${spoken(node)}`}
    >
      {node.href === null ? (
        body
      ) : (
        <Link className="teams-org-link" to={node.href}>
          {body}
        </Link>
      )}
    </g>
  );
}

/** The alphabet `Work.tsx:253` already uses. Two tabs of one bench do not get two of these. */
const MARK: Record<string, string> = {
  done: "✓",
  working: "⋯",
  planned: "·",
  failed: "✗",
  skipped: "–",
};
