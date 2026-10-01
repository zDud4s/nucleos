import { isApiRefusal } from "../data/client";
import {
  RESOLVE_DATA_WARNING,
  useJudgeResolveStatus,
  useSetProjectJudgeResolve,
} from "../data/autopilot";
import { Button, ConfirmButton, ErrorNote, Panel, Quiet, RefusalNote } from "../ui";

/**
 * Spec B (`.ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md`, D11), one project:
 * whether the resolver observes. The warning comes first and is not folded away — it is the
 * reason this is an opt-in. `enforce` is not offered here: the núcleo refuses it until its own
 * warning exists.
 */
export function ResolvePanel({ projectId }: { projectId: string | null }) {
  // Both hooks before the early return (rules of hooks); the query is disabled on `null`.
  const status = useJudgeResolveStatus(projectId);
  const setMode = useSetProjectJudgeResolve();
  if (projectId === null) {
    return (
      <Panel title="Resolving blocks">
        <Quiet says="choose a project above." />
      </Panel>
    );
  }
  const mode = status.data?.judge_resolve ?? "off";
  return (
    <Panel title="Resolving blocks" aside={<span className="ap-project-name">{projectId}</span>}>
      <p className="ap-note" id="resolve-data-warning">
        {RESOLVE_DATA_WARNING}
      </p>
      <div className="ap-actions">
        {mode === "off" ? (
          // Turning it on sends data off the machine, so it is armed, like the judge's Observe.
          <ConfirmButton
            label="Observe how blocks would be resolved"
            confirmLabel="Send blocked commands and gate output to TypeSafe"
            variant="ghost"
            describedBy="resolve-data-warning"
            disabled={setMode.isPending}
            onConfirm={() => setMode.mutate({ project_id: projectId, judge_resolve: "observe" })}
          />
        ) : (
          <Button
            variant="ghost"
            onClick={() => setMode.mutate({ project_id: projectId, judge_resolve: "off" })}
            disabled={setMode.isPending}
          >
            Stop observing
          </Button>
        )}
      </div>
      {setMode.isError && <ResolveRefusal error={setMode.error} />}
    </Panel>
  );
}

function ResolveRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the resolver was not changed</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        unknown_project: "this project is not on the autopilot's roster",
        invalid: "off and observe are the only settings",
        enforce_unavailable: "this núcleo does not accept enforce for the resolver yet",
      }}
    />
  );
}
