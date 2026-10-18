import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import {
  TEAM_RUN_LIST_LIMIT,
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
  Button,
  ConfirmButton,
  Count,
  Crumb,
  ErrorNote,
  Panel,
  Quiet,
  RefusalNote,
  Skeleton,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  StateBadge,
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
  // The tab lives in the address (`?tab=`), so a reload or Back keeps it and "decisions waiting"
  // can be linked. A value this page does not know falls back to Work.
  const search = useSearch({ strict: false }) as Record<string, unknown>;
  const tab = benchTab(search.tab);
  const navigate = useNavigate();
  // Controlled so that a tab can send the reader to another one (Roster -> Charter).
  const setTab = (next: string) =>
    void navigate({
      to: "/teams/$teamId",
      params: { teamId },
      search: ((prev: Record<string, unknown>) => ({
        ...prev,
        tab: next === "work" ? undefined : next,
      })) as never,
      replace: true,
    });

  const team = useTeam(teamId);
  const runs = useTeamRuns();
  const triggers = useTeamTriggers();
  const actions = useOpenTeamActions();

  const detail = team.data;
  const allRuns = runs.data ?? [];
  const teamRuns = allRuns.filter((run) => run.team_id === teamId);
  const teamTriggers = (triggers.data ?? []).filter((rule) => rule.team_id === teamId);
  const waiting = waitingFor(teamId, actions.data ?? [], allRuns);
  // Until the task and decision reads have answered, "idle" would be a guess shown as a fact.
  const unknown = runs.data === undefined || actions.data === undefined;
  const readFailed = runs.isError || actions.isError || triggers.isError;

  if (detail === undefined) {
    return (
      <>
        <Crumb to="/teams">Teams</Crumb>
        <Panel title="Team">
          {team.isError ? (
            <BenchError error={team.error} onRetry={() => void team.refetch()} />
          ) : (
            <Skeleton label="Loading the team" lines={3} />
          )}
        </Panel>
      </>
    );
  }

  const live = teamRuns.filter((run) => teamRunIsAlive(run.state));

  return (
    <>
      <Crumb to="/teams">Teams</Crumb>
      <BenchHead
        team={detail}
        live={live.length}
        waiting={waiting}
        unknown={unknown}
        onReview={() => setTab("decisions")}
      />
      {readFailed && (
        <ErrorNote
          onRetry={() => {
            void runs.refetch();
            void actions.refetch();
            void triggers.refetch();
          }}
        >
          Could not read this team's tasks, decisions or routines, so the numbers here may be incomplete.
        </ErrorNote>
      )}

      <Tabs value={tab} onValueChange={setTab} className="teams-bench-tabs">
        <TabsList aria-label="Team sections">
          <TabsTrigger value="work">Work</TabsTrigger>
          <TabsTrigger value="decisions">
            Decisions
            {/* The count is on the label because this is the tab that is
                waiting for a person — the only one that can be behind. A `Count`
                and not a `Badge`: how many decisions are held is a reading, and the
                project tab strip already says that kind of thing this way. The
                filled capsule stays reserved for a state. */}
            {waiting > 0 && (
              <>
                {" "}
                <Count n={waiting} />
                {" "}
                <span className="sr-only">waiting</span>
              </>
            )}
          </TabsTrigger>
          <TabsTrigger value="roster">Roster</TabsTrigger>
          <TabsTrigger value="routines">Routines</TabsTrigger>
          <TabsTrigger value="charter">Charter</TabsTrigger>
        </TabsList>

        <TabsContent value="work">
          <Work team={detail} runs={teamRuns} capped={allRuns.length >= TEAM_RUN_LIST_LIMIT} />
        </TabsContent>
        <TabsContent value="decisions">
          {/* Every run in the window, not this department's: the tab needs the
              whole list to map `team_run_id` back to a department. */}
          <Decisions team={detail} runs={allRuns} />
        </TabsContent>
        {/* No `forceMount`: Radix unmounting the inactive tab is what keeps the chart from
            polling every live run of this department while nobody is looking at it. */}
        <TabsContent value="roster">
          <Roster team={detail} runs={teamRuns} onOpenCharter={() => setTab("charter")} />
        </TabsContent>
        <TabsContent value="routines">
          <Routines team={detail} rules={teamTriggers} />
        </TabsContent>
        <TabsContent value="charter">
          <Charter team={detail} runs={teamRuns} />
        </TabsContent>
      </Tabs>

      <DeleteTeam team={detail} live={live.length} />
    </>
  );
}

const BENCH_TABS: readonly string[] = ["work", "decisions", "roster", "routines", "charter"];

function benchTab(raw: unknown): string {
  return typeof raw === "string" && BENCH_TABS.includes(raw) ? raw : "work";
}

/**
 * The team's identity and its standing, above the tabs.
 *
 * Deleting used to sit here at the title's weight. It is the end of the team, not part of its
 * identity, so it now lives at the foot of the page ({@link DeleteTeam}).
 */
function BenchHead({
  team,
  live,
  waiting,
  unknown,
  onReview,
}: {
  team: TeamView;
  live: number;
  waiting: number;
  unknown: boolean;
  onReview: () => void;
}) {
  return (
    <header className="teams-bench-head">
      <div className="teams-bench-title">
        <h1 className="teams-bench-name">{team.name}</h1>
        {/* "waiting on you" belongs to Waiting's decision count; this is open team action. */}
        {unknown ? (
          <Badge tone="off">unknown</Badge>
        ) : (
          <StateBadge domain="department" state={live > 0 ? "working" : waiting > 0 ? "waiting" : "idle"} />
        )}
        {waiting > 0 && (
          <Button type="button" onClick={onReview}>
            {waiting === 1 ? "Up to 1 decision is waiting" : `Up to ${waiting} decisions are waiting`} — review
          </Button>
        )}
      </div>
      {team.mission.trim() === "" ? (
        <Quiet says="No description yet — the Charter tab holds the mission." />
      ) : (
        <p className="teams-bench-remit">{team.mission}</p>
      )}
    </header>
  );
}

/**
 * Removing the team, kept away from the title and said in terms of what happens. Unavailable
 * while tasks are running: the hold would otherwise end work in progress on one gesture.
 */
function DeleteTeam({ team, live }: { team: TeamView; live: number }) {
  const del = useDeleteTeam();
  const navigate = useNavigate();
  const noteId = `bench-delete-${team.id}`;
  const them = live === 1 ? "it" : "them";

  return (
    <section className="teams-bench-danger" aria-label="Delete this team">
      <p id={noteId} className="teams-bench-danger-note">
        {live > 0
          ? `${live === 1 ? "1 task is" : `${live} tasks are`} still running. Let ${them} finish, or cancel ${them}, before deleting this team.`
          : "Deleting this team removes it for good. This cannot be undone."}
      </p>
      <ConfirmButton
        label="Delete team"
        confirmLabel="Delete it now"
        subject={team.name}
        variant="danger"
        intent="stop"
        disabled={del.isPending}
        unavailable={live > 0}
        describedBy={noteId}
        onConfirm={() => del.mutate(team.id, { onSuccess: () => void navigate({ to: "/teams" }) })}
      />
      {del.isError && <DeleteRefusal error={del.error} />}
    </section>
  );
}

function BenchError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (isApiRefusal(error)) {
    return (
      <>
        <RefusalNote
          refusal={error}
          sentences={{ not_found: "There is no team with that id.", ...daemonProse(error) }}
        />
        <p>
          <Link to="/teams">Back to teams</Link>
        </p>
      </>
    );
  }
  return (
    <ErrorNote onRetry={onRetry}>
      The {DAEMON_NAME} is not responding, so nothing is known about this team.
    </ErrorNote>
  );
}

function DeleteRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>The {DAEMON_NAME} is not responding, so this team was not deleted.</ErrorNote>;
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
