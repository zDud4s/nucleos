import {
  standingOf,
  STANDING_TONE,
  useProjectBranches,
  useProjectLog,
  type BranchRow,
  type BranchStanding,
} from "../data/project-git";
import { ErrorNote, Quiet, RelativeTime, Row, Rows } from "../ui";

/**
 * Where the work is, and how far it is from landing.
 *
 * State and not current, which is why it belongs on this page at all: `/runs` answers "what has
 * been happening" in time order, and this answers "where do things stand" in structure order. The
 * linear history is a separate question and lives under the Code mode; what is here is a shape.
 *
 * The distances are measured against the branch the **project root** is standing on, because that
 * is what a landing merges into — `land_worktree` computes its target by reading exactly that. A
 * panel that measured against `master` by convention would draw distances to a place nothing merges
 * into, on any project that names its trunk something else.
 */

export interface BranchesProps {
  projectId: string;
}

const STANDING_TEXT: Record<BranchStanding, string> = {
  integration: "where work lands",
  unmeasured: "distance unknown",
  level: "level",
  ahead: "ahead",
  behind: "behind",
  diverged: "diverged",
};

export function Branches({ projectId }: BranchesProps) {
  const branches = useProjectBranches(projectId);
  const recent = useProjectLog(projectId, "", 5);

  if (branches.isError) {
    return (
      <ErrorNote>
        The núcleo could not read this project&rsquo;s branches — its folder may have moved.
      </ErrorNote>
    );
  }
  if (branches.data === undefined) {
    return <p className="text-sm text-text-faint">Reading branches…</p>;
  }

  const { integration, branches: rows, omitted } = branches.data;

  return (
    <div className="flex flex-col gap-4">
      {integration === null ? (
        /*
          A detached root is an ordinary state — somebody checked out a sha to look at something —
          and it is stated rather than papered over, because every distance on this panel depends on
          there being a target and there is not one.
        */
        <p className="text-sm text-tone-paused-fg">
          The project root is on a detached HEAD, so there is no branch for these to be measured
          against.
        </p>
      ) : null}

      {rows.length === 0 ? (
        <Quiet says="No local branches." />
      ) : (
        /*
          `Rows` and not a column of cards, which is the question that picks between the app's two
          ways of presenting a collection: this column is scanned down rather than picked out of.
          It also fixes the rules. The hand-rolled version drew its separators as a 1px gap over
          *nothing*, so what showed through was the page ground — which is a line in dark and
          invisible in light. The shared list paints the border colour behind the gap, and every
          row paints its own fill over it.
        */
        <Rows label="Local branches">
          {rows.map((row) => (
            <Line key={row.name} row={row} integration={integration} />
          ))}
        </Rows>
      )}

      {/*
        A ceiling that hid what it dropped would read as "these are all your branches". Said out
        loud, and only when it happened.
      */}
      {omitted > 0 ? (
        <p className="text-xs text-text-faint">
          {omitted} older {omitted === 1 ? "branch" : "branches"} not measured.
        </p>
      ) : null}

      {recent.data !== undefined && recent.data.length > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="text-xs uppercase tracking-wide text-text-faint">Latest commits</p>
          {recent.data.map((commit) => (
            <p key={commit.sha} className="flex items-baseline gap-2 text-sm">
              <span className="font-mono text-xs text-text-faint">{commit.short_sha}</span>
              <span className="truncate text-text-muted">{commit.subject}</span>
              <span className="ml-auto shrink-0 text-xs text-text-faint">
                <RelativeTime at={commit.at} />
              </span>
            </p>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/*
  Named `Line` and not `Row`, because `Row` is now the shared list item this renders inside. Two
  components of one name in one file is how somebody imports the wrong one and gets a list that
  loses its fill — the row's background is the mechanism that keeps the container's hairline
  ground from showing through, not decoration.
*/
function Line({ row, integration }: { row: BranchRow; integration: string | null }) {
  const standing = standingOf(row, integration);
  const tone = STANDING_TONE[standing];

  return (
    <Row layout="line">
      <span
        className="h-1.5 w-1.5 shrink-0 rounded-pill"
        style={{ background: `var(--tone-${tone}-fg)` }}
        aria-hidden="true"
      />
      <span className="shrink-0 font-mono text-sm text-text">{row.name}</span>

      {/*
        The numbers, and only when there are numbers to give. An unmeasured branch shows the word
        and no digits: a `0/0` beside "unknown" would be two answers to one question.
      */}
      <span className="shrink-0 text-xs text-text-faint">
        {standing === "unmeasured" || standing === "integration"
          ? STANDING_TEXT[standing]
          : `${STANDING_TEXT[standing]}${row.ahead > 0 ? ` +${row.ahead}` : ""}${
              row.behind > 0 ? ` −${row.behind}` : ""
            }`}
      </span>

      <span className="truncate text-sm text-text-muted">{row.last_subject}</span>
      <span className="ml-auto shrink-0 text-xs text-text-faint">
        <RelativeTime at={row.last_commit_at} />
      </span>
    </Row>
  );
}
