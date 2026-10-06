import { useProjectTestsMap } from "../data/project-tests-map";
import { CopyButton } from "../ui/CopyButton";
import { ErrorNote } from "../ui";

/**
 * The project's test map, and the one the daemon would propose.
 *
 * **Read-only on purpose.** The map is a versioned file the repository owns, and the app is not its
 * author: a button that wrote it would be a second author of a document git already owns, and a
 * rewrite through the app would skip the queue every other change to it goes through (spec §3.3).
 * So the page shows the state, offers the proposal to copy, and says where it goes.
 */

export interface TestsMapProps {
  projectId: string;
}

export function TestsMap({ projectId }: TestsMapProps) {
  const map = useProjectTestsMap(projectId);

  if (map.data === undefined) {
    return map.isError ? (
      <ErrorNote>The núcleo did not say what this project's test map is.</ErrorNote>
    ) : (
      <p className="text-sm text-text-faint">Reading the test map…</p>
    );
  }

  const { state, errors, groups, proposal } = map.data;
  const from =
    proposal.sources.length > 0
      ? `from ${proposal.sources.join(", ")}`
      : "no build file recognised";

  return (
    <div className="flex flex-col gap-3">
      {state === "valid" && (
        <p className="text-sm">
          <code>nucleos.tests.yaml</code> — {groups.length} {groups.length === 1 ? "group" : "groups"}
          : {groups.join(", ")}
        </p>
      )}
      {state === "invalid" && (
        <div className="text-sm">
          <p>
            <code>nucleos.tests.yaml</code> is not usable:
          </p>
          <ul className="list-disc pl-5">
            {errors.map((error, index) => (
              <li key={index}>{error}</li>
            ))}
          </ul>
        </div>
      )}
      {state === "absent" && (
        <p className="text-sm">
          No <code>nucleos.tests.yaml</code> yet. Every change runs the whole gate.
        </p>
      )}

      <details open={state === "absent"}>
        <summary className="cursor-pointer text-sm">Proposed map</summary>
        <div className="mt-2 flex flex-col gap-2">
          <p className="text-xs text-text-faint">{from}</p>
          <pre className="overflow-x-auto text-xs">{proposal.yaml}</pre>
          <div>
            <CopyButton value={proposal.yaml} label="the proposed map" />
          </div>
        </div>
      </details>

      <p className="text-xs text-text-faint">
        Commit it at the repository root; it lands through the queue like any change.
      </p>
    </div>
  );
}
