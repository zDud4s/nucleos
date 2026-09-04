import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  teamRunIsAlive,
  useDeleteTeam,
  useOpenTeamActions,
  useTeam,
  useTeamRuns,
  useTeamTriggers,
  type TeamAction,
  type TeamRun,
  type TeamView,
} from "../data/teams";
import {
  Badge,
  ConfirmButton,
  ErrorNote,
  Panel,
  RefusalNote,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "../ui";
import { Charter } from "./Charter";
import { Decisions } from "./Decisions";
import { daemonProse } from "./prose";
import { Work } from "./Work";
import { Roster } from "./Roster";
import { Routines } from "./Routines";
import "../pages/teams.css";

/**
 * The bench — one department, with the whole page to itself.
 *
 * `/teams/$teamId`. The console (`pages/Teams.tsx`) answers *what is there and
 * how is it doing*; this answers *what do I do about this one*, and it gets the
 * full 78rem rather than being a seventh panel stacked under a list.
 *
 * **One tab per API surface, not one tab per topic.** The arrangement is
 * dictated by `PUT /teams/{id}`: it is a full replace — `replace_roster` does a
 * `DELETE` and re-inserts (`core/src/team.rs:427`) — so everything that one
 * call owns has to live in one place under one Save, or saving half of it wipes
 * the other half. Everything else has routes of its own and therefore tabs of
 * its own.
 *
 * | Tab | Owns | Saves |
 * |---|---|---|
 * | `Work` | `GET /team-runs`, `POST /teams/{id}/runs` | nothing — it reads and it starts |
 * | `Decisions` | `GET /team-actions`, `GET /proposals/recruits`, the approve/reject doors | one decision at a time |
 * | `Routines` | `GET`/`POST /team-triggers`, `DELETE`, `/enable`, `/next` | writes and deletes |
 * | `Roster` | `GET /agents`, `GET /teams`, `GET /team-runs/{id}` | nothing — it draws the chart |
 * | `Charter` | `PUT /teams/{id}` | one Save, full replace, with a drift guard |
 */
export function Bench() {
  const params = useParams({ strict: false }) as { teamId?: string };
  const teamId = params.teamId ?? "";

  const team = useTeam(teamId);
  const runs = useTeamRuns();
  const triggers = useTeamTriggers();
  const actions = useOpenTeamActions();

  const detail = team.data;
  const allRuns = runs.data ?? [];
  const teamRuns = allRuns.filter((run) => run.team_id === teamId);
  const teamTriggers = (triggers.data ?? []).filter((rule) => rule.team_id === teamId);
  const waiting = waitingFor(teamId, actions.data ?? [], allRuns);

  if (detail === undefined) {
    return (
      <>
        <BenchCrumb />
        <Panel title="Department">
          {team.isError ? (
            <BenchError error={team.error} />
          ) : (
            <p className="teams-loading">reading the department…</p>
          )}
        </Panel>
      </>
    );
  }

  const live = teamRuns.filter((run) => teamRunIsAlive(run.state));

  return (
    <>
      <BenchCrumb />
      <BenchHead team={detail} live={live.length} waiting={waiting} />

      <Tabs defaultValue="work" className="teams-bench-tabs">
        <TabsList aria-label={`${detail.name} — what to do about it`}>
          <TabsTrigger value="work">Work</TabsTrigger>
          <TabsTrigger value="decisions">
            Decisions
            {/* The count is on the label because this is the tab that is
                waiting for a person — the only one that can be behind. */}
            {waiting > 0 && <Badge tone="pending">{waiting}</Badge>}
          </TabsTrigger>
          <TabsTrigger value="roster">Roster</TabsTrigger>
          <TabsTrigger value="routines">Routines</TabsTrigger>
          <TabsTrigger value="charter">Charter</TabsTrigger>
        </TabsList>

        <TabsContent value="work">
          <Work team={detail} runs={teamRuns} />
        </TabsContent>
        <TabsContent value="decisions">
          {/* Every run in the window, not this department's: the tab needs the
              whole list to map `team_run_id` back to a department. */}
          <Decisions team={detail} runs={allRuns} />
        </TabsContent>
        {/* No `forceMount`: Radix unmounting the inactive tab is what keeps the chart from
            polling every live run of this department while nobody is looking at it. */}
        <TabsContent value="roster">
          <Roster team={detail} runs={teamRuns} />
        </TabsContent>
        <TabsContent value="routines">
          <Routines team={detail} rules={teamTriggers} />
        </TabsContent>
        <TabsContent value="charter">
          <Charter team={detail} runs={teamRuns} />
        </TabsContent>
      </Tabs>
    </>
  );
}

function BenchCrumb() {
  return (
    <p className="teams-crumb">
      <Link to="/teams">← Teams</Link>
    </p>
  );
}

/**
 * The department's identity and its standing, above the tabs.
 *
 * Deleting lives here rather than in the Charter: the Charter is about what
 * this department IS, and removing it is not an edit of that — it is the end of
 * it. `ConfirmButton` because it is a destructive write, which is the standing
 * rule for the interlock.
 */
function BenchHead({ team, live, waiting }: { team: TeamView; live: number; waiting: number }) {
  const del = useDeleteTeam();
  const navigate = useNavigate();

  return (
    <header className="teams-bench-head">
      <div className="teams-bench-title">
        <h1 className="teams-bench-name">{team.name}</h1>
        {live > 0 ? (
          <Badge tone="active">at work</Badge>
        ) : waiting > 0 ? (
          <Badge tone="pending">waiting on you</Badge>
        ) : (
          <Badge tone="off">idle</Badge>
        )}
      </div>
      <p className="teams-bench-remit">{team.mission}</p>
      <div className="teams-bench-aside">
        <ConfirmButton
          label="Delete department"
          confirmLabel="Delete it now"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(team.id, { onSuccess: () => void navigate({ to: "/teams" }) })}
        />
      </div>
      {del.isError && <DeleteRefusal error={del.error} />}
    </header>
  );
}

function BenchError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "there is no department with that id", ...daemonProse(error) }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this department</ErrorNote>;
}

function DeleteRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this department was not deleted</ErrorNote>;
}

/**
 * How many decisions this department is holding.
 *
 * `GET /team-actions` carries `team_run_id` and no department, so the link back
 * is through the run list — which is the newest hundred across every
 * department. An action whose run has fallen off that window is counted
 * nowhere rather than counted wrongly, and the count is in any case an upper
 * bound: the route answers `pending` AND `working` (`core/src/team.rs:3570`)
 * while the daemon's own ceiling counts only `pending` with an undecided
 * proposal (`open_actions_of`, `team.rs:1764`).
 */
export function waitingFor(teamId: string, actions: TeamAction[], runs: TeamRun[]): number {
  const mine = new Set(runs.filter((run) => run.team_id === teamId).map((run) => run.id));
  return actions.filter((action) => mine.has(action.team_run_id)).length;
}
