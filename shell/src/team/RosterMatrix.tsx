import type { TeamView } from "../data/teams";

/**
 * Who works where — every specialist against every department.
 *
 * The fact this draws was already in the app and was visible nowhere.
 * `team_members` is `(team_id, agent_id)` with no uniqueness across teams
 * (`core/src/team.rs:434`), so one specialist can serve several departments at
 * once; the old list showed a department's roster only once you had opened that
 * department, which meant the shape of the whole organisation was something you
 * had to hold in your head.
 *
 * **A matrix and not a graph.** Nine specialists against six departments is
 * fifty-four cells and reads at a glance; the same relation as a node graph is a
 * hairball that has to be untangled before it says anything. The graph belongs
 * inside a single task, where there is real flow — an order, a dependency, a
 * handoff — rather than a membership.
 *
 * **It costs one call.** `GET /teams` already returns every department with its
 * roster and its grants attached (`list_teams`, `core/src/team.rs:445-462`), so
 * this whole panel is a rearrangement of a query the page was already making.
 * Nothing here fetches, and nothing here should start to.
 *
 * A real `<table>`, because that is what it is: a specialist is a row header, a
 * department is a column header, and a screen reader that lands in a cell is
 * told both. Built out of `div`s it would be a picture of a table that only a
 * sighted user can read.
 */

export interface RosterMatrixProps {
  teams: TeamView[];
}

/** One person's standing in one department. Three states, and absence is one of them. */
type Standing = "leads" | "staff" | "none";

const SAID: Record<Standing, string> = {
  leads: "directs",
  staff: "on staff",
  none: "not on staff",
};

/** The mark each standing gets. Never colour alone — the glyph carries it. */
const MARK: Record<Standing, string> = { leads: "◉", staff: "●", none: "·" };

function standingOf(team: TeamView, agentId: string): Standing {
  if (team.director_agent_id === agentId) return "leads";
  return team.members.includes(agentId) ? "staff" : "none";
}

/**
 * Every specialist any department names, directors included.
 *
 * The union of the rosters with the directors, and not just the rosters: a
 * director is not necessarily in `members` — the daemon stores the two
 * separately — so a matrix built from `members` alone would leave out precisely
 * the person whose absence stops a task from starting.
 *
 * Sorted so the shape is stable across polls and legible on arrival: whoever
 * directs something first, then whoever serves the most departments, then
 * alphabetically. All three are needed — the first two leave ties.
 */
export function specialistsOf(teams: TeamView[]): string[] {
  const everyone = new Set<string>();
  for (const team of teams) {
    for (const member of team.members) everyone.add(member);
    if (team.director_agent_id !== "") everyone.add(team.director_agent_id);
  }

  const leads = new Set(teams.map((team) => team.director_agent_id));
  const spread = new Map<string, number>();
  for (const id of everyone) {
    spread.set(id, teams.filter((team) => standingOf(team, id) !== "none").length);
  }

  return [...everyone].sort((a, b) => {
    const byLead = Number(leads.has(b)) - Number(leads.has(a));
    if (byLead !== 0) return byLead;
    const bySpread = (spread.get(b) ?? 0) - (spread.get(a) ?? 0);
    if (bySpread !== 0) return bySpread;
    return a.localeCompare(b);
  });
}

/** How many people a department has, counting its director whether or not the roster does. */
export function headcountOf(team: TeamView): number {
  return new Set([...team.members, ...(team.director_agent_id === "" ? [] : [team.director_agent_id])]).size;
}

export function RosterMatrix({ teams }: RosterMatrixProps) {
  const specialists = specialistsOf(teams);

  /*
    A department with nobody in it is a row of empty cells, not a missing column.
    That is the state a department is in for the minute between being created and
    being staffed, and it is also the state that stops every task it is asked to
    run — so it is the one column most worth being able to see.
  */
  if (teams.length === 0 || specialists.length === 0) {
    return (
      <p className="teams-empty">
        {teams.length === 0
          ? "no department has been created yet."
          : "no department has anybody in it yet — a task cannot start without a roster."}
      </p>
    );
  }

  return (
    /*
      The scroller is the wrapper and not the page. Wide content scrolls inside
      its own box; a table that made the whole document slide sideways would take
      the sidebar and the header with it.
    */
    <div className="teams-matrix-scroll">
      <table className="teams-matrix">
        <caption className="teams-matrix-caption">
          {specialists.length} {specialists.length === 1 ? "specialist" : "specialists"} across{" "}
          {teams.length} {teams.length === 1 ? "department" : "departments"}. ◉ directs · ● on staff
        </caption>
        <thead>
          <tr>
            <th className="teams-matrix-corner" scope="col">
              <span className="teams-matrix-said">Specialist</span>
            </th>
            {teams.map((team) => (
              <th className="teams-matrix-col" scope="col" key={team.id}>
                {team.name}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {specialists.map((agentId) => (
            <tr key={agentId}>
              <th className="teams-matrix-row" scope="row">
                {agentId}
              </th>
              {teams.map((team) => {
                const standing = standingOf(team, agentId);
                return (
                  <td
                    className={`teams-matrix-cell teams-matrix-${standing}`}
                    key={team.id}
                    title={`${agentId} ${SAID[standing]} ${team.name}`}
                  >
                    <span aria-hidden="true">{MARK[standing]}</span>
                    {/* The glyph is for the eye; this is the same fact in words. */}
                    <span className="teams-matrix-said">{SAID[standing]}</span>
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
        <tfoot>
          <tr>
            <th className="teams-matrix-row" scope="row">
              headcount
            </th>
            {teams.map((team) => (
              <td className="teams-matrix-count" key={team.id}>
                {headcountOf(team)}
              </td>
            ))}
          </tr>
        </tfoot>
      </table>
    </div>
  );
}
