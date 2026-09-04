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
import { buildRoster, clip, placeRoster, type RosterNode } from "./roster-graph";

/**
 * `Roster` — the department drawn as its own org chart.
 *
 * The director on top, the specialists under it, and under each of those whatever it is holding
 * right now. Read-only: nothing here writes, and the only thing it can be clicked into is the task
 * a piece of work belongs to.
 *
 * **Why this is a graph when `RosterMatrix.tsx` decided a roster is a matrix.** That decision was
 * about the whole house — nine specialists against six departments — and it still stands, and the
 * console still draws it. What it left open is the case this tab takes: one department, with the
 * work in flight hanging off the person holding it. That third rank is a relation, not a
 * membership, and a matrix has no cell for it.
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
    return <p className="teams-loading">reading the catalogue…</p>;
  }

  const alone = !model.nodes.some((node) => node.layer === 1);
  const inFlight = model.nodes.some((node) => node.layer === 2);
  const waiting = live.length > 0 && flight.pending && flight.runs.length === 0;

  return (
    <div className="teams-org">
      <div className="teams-org-scroll">
        {/*
          `role="group"` and not the `role="img"` the fleet's graph carries. An image's children are
          presentational, and half the boxes here are links — calling this a picture would take the
          work out of the accessibility tree along with the way into it.
        */}
        <svg
          className="teams-org-svg"
          role="group"
          aria-label={`${team.name} — its director, its roster and the work in flight`}
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
              return (
                <path
                  key={`${edge.from}->${edge.to}`}
                  className={edge.crosses ? "teams-org-edge teams-org-edge-far" : "teams-org-edge"}
                  d={`M ${x1} ${y1} C ${x1} ${y1 + 22}, ${x2} ${y2 - 22}, ${x2} ${y2}`}
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

      {alone && (
        <p className="teams-org-empty">
          no roster yet — this department is a director and nobody else.
        </p>
      )}
      {waiting && <p className="teams-org-empty">reading the work…</p>}
      {!waiting && !inFlight && <p className="teams-org-empty">nothing in flight</p>}
    </div>
  );
}

/**
 * One box, and the one fact it adds to its own name.
 *
 * A person at the top directs; a specialist is what it does, or that the catalogue no longer has
 * it; a piece of work is its state. Those are three different sentences, and each is the one thing
 * somebody would ask about that box — which is why this is not a single field.
 */
function spoken(node: RosterNode): string {
  return node.layer === 1 ? node.said : node.state;
}

interface BoxProps {
  node: RosterNode;
  box: { x: number; y: number; width: number; height: number; lines: string[] };
}

function Box({ node, box }: BoxProps) {
  const kind =
    node.layer === 0 ? "teams-org-lead" : node.layer === 1 ? "teams-org-who" : "teams-org-item";
  const tone = node.layer === 2 ? ` teams-org-${node.state}` : "";
  const gone = node.missing ? " teams-org-missing" : "";

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
      <text
        className="teams-org-said"
        x={box.width / 2}
        y={node.layer === 2 ? 49 : box.height - 12}
        textAnchor="middle"
      >
        {clip(node.said)}
      </text>
      {node.layer === 2 && (
        <text className="teams-org-state" x={box.width / 2} y={65} textAnchor="middle">
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
      className={`teams-org-node ${kind}${tone}${gone}`}
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
