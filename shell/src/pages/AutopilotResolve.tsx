import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  RESOLVE_DATA_WARNING,
  RESOLVE_ENFORCE_RISK,
  TYPESAFE_DEFINITION,
  TYPESAFE_LEAVES,
  formatProbability,
  outcomesFor,
  readOutcome,
  readReadiness,
  useJudgeResolutions,
  useJudgeResolveStatus,
  useSetProjectJudgeResolve,
  useSetResolutionOutcome,
  type Resolution,
  type ResolveOutcome,
} from "../data/autopilot";
import type { ProjectSummary } from "../data/system";
import { Badge, Button, ConfirmButton, Count, ErrorNote, Inset, Panel, Quiet, RefusalNote, RelativeTime } from "../ui";
import { DecisionDialog } from "./AutopilotJudge";

/**
 * Spec B (`.ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md`, D11), one project:
 * whether the resolver observes or decides, how far it is from its bar, and the queue that gets
 * it there. The data warning comes first and is not folded away; the enforce risk (section 5) is
 * on screen whenever enforce could be chosen, and the button names it.
 */
export function ResolvePanel({
  projectId,
  project,
}: {
  projectId: string | null;
  project: ProjectSummary | undefined;
}) {
  // Every hook before the early return (rules of hooks); the queries are disabled on `null`.
  const status = useJudgeResolveStatus(projectId);
  const queue = useJudgeResolutions(projectId);
  const setMode = useSetProjectJudgeResolve();
  const setOutcome = useSetResolutionOutcome();
  /** Whether the dialog for turning observing on is open: it sends data off this computer. */
  const [asking, setAsking] = useState(false);
  if (projectId === null) {
    return (
      <Panel title="Resolving blocks">
        <Quiet says="Choose a project above." />
      </Panel>
    );
  }
  const mode = status.data?.judge_resolve ?? "off";
  const readiness = status.data?.readiness;
  // The núcleo refuses enforce off Active or under the bar; the button is shut to match (spec B D11).
  const enforceShut = project?.mode !== "active" || readiness?.ready !== true;
  const rows = queue.data ?? [];
  return (
    <Panel title="Resolving blocks" aside={<span className="ap-project-name">{projectId}</span>}>
      <p className="ap-note" id="resolve-data-warning">
        {RESOLVE_DATA_WARNING}
      </p>
      {readiness !== undefined && <p className="ap-note">{readReadiness(readiness)}</p>}
      {mode !== "off" && (
        <p className="ap-note" id="resolve-enforce-risk">
          {RESOLVE_ENFORCE_RISK}
        </p>
      )}
      <div className="ap-actions">
        {mode === "off" ? (
          // Turning it on sends data off the machine, so it asks in a dialog, like the judge's watching.
          <Button
            variant="ghost"
            aria-describedby="resolve-data-warning"
            disabled={setMode.isPending}
            onClick={() => setAsking(true)}
          >
            Observe how blocks would be resolved
          </Button>
        ) : (
          <Button
            variant="ghost"
            onClick={() => setMode.mutate({ project_id: projectId, judge_resolve: mode === "enforce" ? "observe" : "off" })}
            disabled={setMode.isPending}
          >
            {mode === "enforce" ? "Back to observing" : "Stop observing"}
          </Button>
        )}
        {mode === "observe" && (
          <ConfirmButton
            label="Let the resolver decide"
            confirmLabel="Let the resolver decide — I accept the risk above"
            variant="danger"
            describedBy="resolve-enforce-risk"
            disabled={setMode.isPending || enforceShut}
            onConfirm={() => setMode.mutate({ project_id: projectId, judge_resolve: "enforce" })}
          />
        )}
      </div>
      {setMode.isError && <ResolveRefusal error={setMode.error} />}
      {/* An empty queue is one quiet line; the heading and its count appear with the first block. */}
      {queue.data !== undefined && rows.length === 0 && (
        <Quiet says="No block the resolver saw is waiting for you." />
      )}
      {rows.length > 0 && (
        <>
          <p className="ap-note">
            <strong>Blocks to review</strong> <Count n={rows.length} />
          </p>
          <ul className="ap-list" aria-label="Resolver blocks">
            {rows.map((item) => (
              <ResolutionCard
                key={item.id}
                item={item}
                busy={setOutcome.isPending}
                onAnswer={(outcome) => setOutcome.mutate({ id: item.id, outcome })}
              />
            ))}
          </ul>
        </>
      )}
      <DecisionDialog
        open={asking}
        onOpenChange={setAsking}
        title="Send this project's blocked commands to TypeSafe?"
        description={`${TYPESAFE_DEFINITION} If you continue, ${TYPESAFE_LEAVES}, plus the end of the gate output when a run fails its gate. Observing changes nothing a run does.`}
        confirmLabel="Start observing"
        confirmVariant="approve"
        onConfirm={() => setMode.mutate({ project_id: projectId, judge_resolve: "observe" })}
      />
      {setOutcome.isError && <ErrorNote>the review was not recorded</ErrorNote>}
    </Panel>
  );
}

/** The four questions, each only when it was asked of this event. */
const QUESTIONS = [
  ["off task", "p_off_task"],
  ["needed", "p_needed"],
  ["avoidable", "p_avoidable"],
  ["fixable", "p_fixable"],
] as const;

function ResolutionCard({
  item,
  busy,
  onAnswer,
}: {
  item: Resolution;
  busy: boolean;
  onAnswer: (outcome: ResolveOutcome) => void;
}) {
  const gate = item.event === "gate_failed";
  const raw = gate ? item.gate_output : item.tool_input;
  return (
    <Inset as="li">
      <div className="ap-card-head">
        <span className="ap-card-id">block #{item.id}</span>
        <span className="ap-card-title">{gate ? "failed gate" : item.tool_name}</span>
        <Badge tone="pending">judge: {readOutcome(item.judge_outcome)}</Badge>
        <RelativeTime at={item.created_at} />
      </div>
      <dl className="ap-decision-facts">
        {QUESTIONS.filter(([, key]) => item[key] !== null).map(([label, key]) => (
          <div className="ap-decision-fact" key={key}>
            <dt>{label}</dt>
            <dd>{formatProbability(item[key])}</dd>
          </div>
        ))}
        <div className="ap-decision-fact">
          <dt>run</dt>
          <dd>
            <Link className="ap-link" to={`/runs/${item.run_id}`}>
              run {item.run_id}
            </Link>
          </dd>
        </div>
      </dl>
      {raw !== null && (
        <pre className="ap-raw">
          <code>{raw}</code>
        </pre>
      )}
      <div className="ap-actions">
        {outcomesFor(item.event).map((outcome) => (
          <ConfirmButton
            key={outcome}
            label={readOutcome(outcome)}
            confirmLabel={`Yes — ${readOutcome(outcome)}`}
            variant="ghost"
            disabled={busy}
            onConfirm={() => onAnswer(outcome)}
          />
        ))}
      </div>
    </Inset>
  );
}

function ResolveRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>The núcleo did not answer — the resolver was not changed.</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_active: "the resolver decides only on top of an active project",
        not_ready: "the resolver has not cleared its bar yet — review more of its blocks below",
        unknown_project: "this project is not on the autopilot's roster",
        invalid: "off, observe and enforce are the only settings",
        // A shell newer than its núcleo: until the núcleo shows the risk, the route still says this.
        enforce_unavailable:
          "this núcleo does not accept enforce for the resolver yet — update it to one that shows the risk first",
      }}
    />
  );
}
