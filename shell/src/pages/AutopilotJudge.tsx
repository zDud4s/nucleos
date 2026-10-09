import { useId, useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  JUDGE_ENFORCE_DIALOG,
  JUDGE_RESIDUAL_RISK,
  READINESS_MIN_AGREE_PERCENT,
  READINESS_MIN_REVIEWED,
  TYPESAFE_DEFINITION,
  TYPESAFE_LEAVES,
  formatProbability,
  readJudgeBand,
  readJudgeOpinion,
  useRunJudgeOpinions,
  useJudgeStatus,
  useJudgeVerdicts,
  useSetJudgeVerdict,
  useSetProjectJudge,
  type JudgeMode,
  type JudgeOpinion,
  type JudgeVerdict,
} from "../data/autopilot";
import type { ProjectSummary } from "../data/system";
import {
  Badge,
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  Inset,
  Modal,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Section,
} from "../ui";

/**
 * Spec A, on the Autopilot page: whether a model is asked about this project's tool calls, how
 * far it is from being allowed to decide, and the queue that gets it there.
 *
 * Every number is the núcleo's (`judge::readiness`); this file only shows them.
 */

/** How the judge's three modes read on the switch: the project switch's verbs, one set for the page. */
const MODE_LABEL: Record<JudgeMode, string> = {
  off: "Turn off",
  observe: "Watch in shadow",
  enforce: "Let it act",
};

/** How the current mode reads in a sentence. The daemon's values (`off/observe/enforce`) are unchanged. */
const MODE_NOW: Record<JudgeMode, string> = {
  off: "off",
  observe: "watching in shadow",
  enforce: "acting",
};

/**
 * A decision that deserves more than a second click: a real dialog, Cancel first and the
 * consequential answer last. Cancel is the first footer button, so it is where focus lands.
 * Used for the two changes that cost something outside the app: data leaving the machine, and
 * a risk the owner accepts.
 */
export function DecisionDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  confirmVariant,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: ReactNode;
  confirmLabel: string;
  confirmVariant: "approve" | "danger-solid";
  onConfirm: () => void;
}) {
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      description={description}
      footer={
        <>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            variant={confirmVariant}
            onClick={() => {
              onOpenChange(false);
              onConfirm();
            }}
          >
            {confirmLabel}
          </Button>
        </>
      }
    />
  );
}

export function JudgePanel({
  projectId,
  project,
}: {
  projectId: string | null;
  project: ProjectSummary | undefined;
}) {
  const status = useJudgeStatus(projectId);
  const setJudge = useSetProjectJudge();
  /** The change waiting on its dialog: only the two that cost something outside the app open one. */
  const [asking, setAsking] = useState<"observe" | "enforce" | null>(null);
  const prereqId = useId();
  if (projectId === null) {
    return (
      <Panel title="Judge">
        <Quiet says="Choose a project above." />
      </Panel>
    );
  }
  const current = status.data?.judge;
  const readiness = status.data?.readiness;
  const active = project?.mode === "active";
  const enforceShut = !active || readiness?.ready !== true;
  const busy = setJudge.isPending;
  // What stands between the owner and the third segment, said under the control it locks.
  const needs: string[] = [];
  if (!active) needs.push("an active project");
  if (readiness?.ready !== true) {
    needs.push(
      `${READINESS_MIN_REVIEWED} reviewed at ${READINESS_MIN_AGREE_PERCENT}% agreement` +
        (readiness === undefined ? "" : ` (${readiness.reviewed} reviewed so far, ${readiness.agree} agreed)`),
    );
  }
  const apply = (mode: JudgeMode) => setJudge.mutate({ project_id: projectId, judge: mode });
  return (
    <Panel title="Judge" aside={<span className="ap-project-name">{projectId}</span>}>
      <p className="ap-note">
        A model asked about each tool call the classifier left open. Watching in shadow sends each
        such call to TypeSafe — {TYPESAFE_LEAVES} — and changes nothing; letting it act lets its
        answer stand, except for the classes and guards it may refuse but never approve.{" "}
        {TYPESAFE_DEFINITION}
      </p>
      {status.isError && status.data === undefined && (
        <ErrorNote>The núcleo did not answer — nothing is known about the judge.</ErrorNote>
      )}
      {current !== undefined && (
        <p className="ap-reason">
          The judge is <strong>{MODE_NOW[current]}</strong> for {projectId}.
        </p>
      )}
      <div className="ap-judge-control">
        {/* One switch, the project switch's shape: the current mode is pressed, never disabled, and
            a mode that is not open says why right under the control. */}
        <div className="ui-switch" role="group" aria-label="Judge mode">
          {current === "off" ? (
            <button type="button" className="ui-switch-seg" aria-pressed>
              {MODE_LABEL.off}
            </button>
          ) : (
            <span className="ui-switch-seg-wrap">
              <ConfirmButton
                label={MODE_LABEL.off}
                confirmLabel="Stop asking the judge"
                variant="ghost"
                disabled={busy}
                onConfirm={() => apply("off")}
              />
            </span>
          )}
          <button
            type="button"
            className="ui-switch-seg"
            aria-pressed={current === "observe"}
            aria-disabled={busy ? "true" : undefined}
            onClick={() => {
              if (busy || current === "observe") return;
              setAsking("observe");
            }}
          >
            {MODE_LABEL.observe}
          </button>
          <button
            type="button"
            className="ui-switch-seg"
            aria-pressed={current === "enforce"}
            aria-disabled={busy || (enforceShut && current !== "enforce") ? "true" : undefined}
            aria-describedby={enforceShut ? prereqId : "judge-residual-risk"}
            onClick={() => {
              if (busy || enforceShut || current === "enforce") return;
              setAsking("enforce");
            }}
          >
            {MODE_LABEL.enforce}
          </button>
        </div>
        {enforceShut && (
          <p id={prereqId} className="ap-reason">
            Letting the judge act needs {needs.join(" and ")}.
          </p>
        )}
      </div>
      {setJudge.isError && <JudgeRefusal error={setJudge.error} />}
      {/* Review item E(i): the setting is photographed onto each run at launch (D2). */}
      <p className="ap-note">
        A change here reaches this project's next runs; runs already working keep the setting they
        started with.
      </p>
      {/* Review item G: fails closed (the classifier decides alone), and would otherwise be silent. */}
      {status.data?.rules_error != null && (
        <ErrorNote>
          {`This project's rules cannot be read — the judge has no effect until they can: ${status.data.rules_error}`}
        </ErrorNote>
      )}
      {readiness !== undefined && (
        <>
          <p className="ap-note">
            {readiness.reviewed} distinct actions reviewed, {readiness.agree} agreed — letting it act
            needs {READINESS_MIN_REVIEWED} at {READINESS_MIN_AGREE_PERCENT}%, on an active project.
          </p>
          {readiness.by_class.length > 0 && (
            <dl className="ap-decision-facts" aria-label="Agreement by class">
              {readiness.by_class.map((row) => (
                <div className="ap-decision-fact" key={row.action_class}>
                  <dt>{row.action_class}</dt>
                  <dd>
                    {row.agree} of {row.reviewed}
                  </dd>
                </div>
              ))}
            </dl>
          )}
        </>
      )}
      <p className="ap-note" id="judge-residual-risk">
        {JUDGE_RESIDUAL_RISK}
      </p>
      <DecisionDialog
        open={asking === "observe"}
        onOpenChange={(open) => !open && setAsking(null)}
        title="Send this project's commands to TypeSafe?"
        description={`${TYPESAFE_DEFINITION} If you continue, ${TYPESAFE_LEAVES}. The judge only watches: it changes nothing a run does.`}
        confirmLabel={MODE_LABEL.observe}
        confirmVariant="approve"
        onConfirm={() => apply("observe")}
      />
      <DecisionDialog
        open={asking === "enforce"}
        onOpenChange={(open) => !open && setAsking(null)}
        title={`Let the judge approve commands for ${projectId}?`}
        description={`${JUDGE_ENFORCE_DIALOG} Turning this on accepts that risk for this project.`}
        confirmLabel={MODE_LABEL.enforce}
        confirmVariant="danger-solid"
        onConfirm={() => apply("enforce")}
      />
    </Panel>
  );
}

function JudgeRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the judge was not changed</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_active: "the judge decides only on top of an active project",
        not_ready: "the judge has not cleared the bar yet — review more of its verdicts below",
        unknown_project: "this project is not on the autopilot's roster",
        invalid: "off, observe and enforce are the only settings",
        // A shell newer than its núcleo: until the núcleo has Task 9.5, the route still says this.
        enforce_unavailable: "this núcleo does not accept enforce yet — update it to one that shows the residual risk first",
      }}
    />
  );
}

export function JudgeReviewPanel({ projectId }: { projectId: string | null }) {
  const verdicts = useJudgeVerdicts(projectId);
  const answer = useSetJudgeVerdict();
  const rows = verdicts.data ?? [];
  // An empty queue is a quiet line, like the other review panels, not a panel with a zero in it.
  if (projectId !== null && verdicts.data !== undefined && rows.length === 0) {
    return (
      <Section label="Judge verdicts">
        <Quiet says="No verdict of the judge is waiting for you." />
      </Section>
    );
  }
  return (
    <Panel title="Judge verdicts" aside={<Count n={projectId === null ? undefined : rows.length} />}>
      {projectId === null && <Quiet says="Choose a project above." />}
      {rows.length > 0 && (
        <ul className="ap-list" aria-label="Judge verdicts">
          {rows.map((verdict) => (
            <VerdictCard
              key={verdict.id}
              verdict={verdict}
              busy={answer.isPending}
              onAnswer={(choice) => answer.mutate({ verdictId: verdict.id, verdict: choice })}
            />
          ))}
        </ul>
      )}
      {answer.isError && <ErrorNote>the verdict was not recorded</ErrorNote>}
    </Panel>
  );
}

function VerdictCard({
  verdict,
  busy,
  onAnswer,
}: {
  verdict: JudgeVerdict;
  busy: boolean;
  onAnswer: (choice: "approve" | "reject") => void;
}) {
  return (
    <Inset as="li">
      <div className="ap-card-head">
        <span className="ap-card-id">verdict #{verdict.id}</span>
        <span className="ap-card-title">{verdict.tool_name}</span>
        <Badge tone="info">{verdict.action_class}</Badge>
        <Badge tone={verdict.band === "deny" ? "danger" : "pending"}>{readJudgeBand(verdict)}</Badge>
        <RelativeTime at={verdict.created_at} />
      </div>
      <dl className="ap-decision-facts">
        <div className="ap-decision-fact">
          <dt>the judge</dt>
          <dd>{formatProbability(verdict.p)}</dd>
        </div>
        <div className="ap-decision-fact">
          <dt>in scope · safe</dt>
          <dd>
            {formatProbability(verdict.p_in_scope)} · {formatProbability(verdict.p_safe)}
          </dd>
        </div>
        <div className="ap-decision-fact">
          <dt>the classifier</dt>
          <dd>{verdict.classifier_decision}</dd>
        </div>
        <div className="ap-decision-fact">
          <dt>run</dt>
          <dd>
            <Link className="ap-link" to={`/runs/${verdict.run_id}`}>
              run {verdict.run_id}
            </Link>
          </dd>
        </div>
      </dl>
      {verdict.tool_input !== null && (
        <pre className="ap-raw">
          <code>{verdict.tool_input}</code>
        </pre>
      )}
      <div className="ap-actions">
        <ConfirmButton
          label={`Agree #${verdict.id}`}
          confirmLabel="The judge was right"
          variant="approve"
          disabled={busy}
          onConfirm={() => onAnswer(verdict.band === "allow" ? "approve" : "reject")}
        />
        <ConfirmButton
          label={`Disagree #${verdict.id}`}
          confirmLabel="The judge was wrong"
          variant="ghost"
          disabled={busy}
          onConfirm={() => onAnswer(verdict.band === "allow" ? "reject" : "approve")}
        />
      </div>
    </Inset>
  );
}

/** The judge's word on one decision, as one fact line. */
export function JudgeOpinionLine({ opinion }: { opinion: JudgeOpinion }) {
  return (
    <div className="ap-decision-fact">
      <dt>the judge</dt>
      <dd>{readJudgeOpinion(opinion)}</dd>
    </div>
  );
}

/** A run's verdicts, oldest first — the run page's "what the judge said". Nothing when there are none. */
export function JudgeOpinionsBlock({ runId }: { runId: number }) {
  const opinions = useRunJudgeOpinions(runId);
  const rows = opinions.data ?? [];
  if (rows.length === 0) return null;
  return (
    <Panel title="What the judge said">
      <dl className="ap-decision-facts">
        {rows.map((opinion) => (
          <div className="ap-decision-fact" key={opinion.id}>
            <dt>
              {opinion.tool_name} · <RelativeTime at={opinion.created_at} />
            </dt>
            <dd>{readJudgeOpinion(opinion)}</dd>
          </div>
        ))}
      </dl>
    </Panel>
  );
}
