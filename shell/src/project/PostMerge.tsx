import type { ProjectRules } from "../data/projects";
import { Panel } from "../ui";
/* The `pj-` family, as in `OnItsOwn`: the panel draws correctly wherever it is mounted. */
import "../pages/projects.css";

function Sha({ value }: { value: string }) {
  return <code className="pj-verbatim">{value.slice(0, 12)}</code>;
}

function ShaList({ values }: { values: string[] }) {
  return (
    <>
      {values.map((sha, i) => (
        <span key={sha}>
          {i > 0 && ", "}
          <Sha value={sha} />
        </span>
      ))}
    </>
  );
}

/**
 * The post-merge state of the project's target, read only.
 *
 * It draws what the rules read reported and nothing else: no control, no request. A project whose
 * gate has never run on its target (`postgate` null, or absent from an older daemon) draws nothing.
 * When the daemon could not read the state (`postgate` null with `postgate_error` set) it says so
 * and shows the error, rather than looking the same as "never run".
 */
export function PostMergePanel({ rules }: { rules: ProjectRules }) {
  const state = rules.postgate;
  if (state == null) {
    if (rules.postgate_error == null) return null;
    return (
      <Panel title="Post-merge gate">
        <p className="pj-wip-state">The post-merge state could not be read.</p>
        <p className="pj-note">{rules.postgate_error}</p>
      </Panel>
    );
  }

  const red = state.red_groups.length > 0;

  return (
    <Panel title="Post-merge gate">
      {red ? (
        <>
          <p className="pj-wip-state">
            <strong>{state.target}</strong> is red
            {state.red_sha != null && (
              <>
                {" "}at <Sha value={state.red_sha} />
              </>
            )}
            {state.red_since != null && (
              <>
                , since <Sha value={state.red_since} />
              </>
            )}
            .
          </p>
          <p className="pj-note">Failing: {state.red_groups.join(", ")}</p>
          {state.phase === "flake_check" && (
            <p className="pj-note">
              Rechecking the same commit to rule out an unstable test.
            </p>
          )}
          {state.phase === "bisect" && (
            <p className="pj-note">Looking for the merge that broke it.</p>
          )}
          {state.culprit != null ? (
            <p className="pj-note">
              Culprit: <Sha value={state.culprit} />
            </p>
          ) : (
            state.candidates.length > 0 && (
              <p className="pj-note">
                Inconclusive between: <ShaList values={state.candidates} />
              </p>
            )
          )}
          {state.red_sha != null && (
            <p className="pj-note">
              {state.red_base != null ? (
                <>
                  Range: after <Sha value={state.red_base} />, up to the red commit.
                </>
              ) : (
                "Range: every commit up to the red one."
              )}
            </p>
          )}
          {state.also_suspect.length > 0 && (
            <p className="pj-note">
              Also suspect: <ShaList values={state.also_suspect} />
            </p>
          )}
        </>
      ) : (
        <p className="pj-wip-state">
          {state.last_green != null ? (
            <>
              <strong>{state.target}</strong> is green at <Sha value={state.last_green} />.
            </>
          ) : (
            <>No result on <strong>{state.target}</strong> yet.</>
          )}
        </p>
      )}
      {state.running != null && (
        <p className="pj-note">
          A gate is running on <Sha value={state.running} />.
        </p>
      )}
    </Panel>
  );
}
