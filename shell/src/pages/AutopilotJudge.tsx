import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  JUDGE_RESIDUAL_RISK,
  READINESS_MIN_AGREE_PERCENT,
  READINESS_MIN_REVIEWED,
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
import { Badge, ConfirmButton, Count, ErrorNote, Inset, Panel, Quiet, RefusalNote, RelativeTime } from "../ui";

/**
 * Spec A, on the Autopilot page: whether a model is asked about this project's tool calls, how
 * far it is from being allowed to decide, and the queue that gets it there.
 *
 * Every number is the núcleo's (`judge::readiness`); this file only shows them.
 */

const MODES: { mode: JudgeMode; label: string; confirm: string }[] = [
  { mode: "off", label: "Off", confirm: "Stop asking the judge" },
  // D11: observing is opt-in because it sends each judged call off this machine.
  { mode: "observe", label: "Observe", confirm: "Send each judged call to TypeSafe" },
  { mode: "enforce", label: "Enforce", confirm: "Let the judge decide — I accept the risk above" },
];

export function JudgePanel({
  projectId,
  project,
}: {
  projectId: string | null;
  project: ProjectSummary | undefined;
}) {
  const status = useJudgeStatus(projectId);
  const setJudge = useSetProjectJudge();
  if (projectId === null) {
    return (
      <Panel title="Judge">
        <Quiet says="choose a project above." />
      </Panel>
    );
  }
  const current = status.data?.judge;
  const readiness = status.data?.readiness;
  const active = project?.mode === "active";
  const enforceShut = !active || readiness?.ready !== true;
  return (
    <Panel title="Judge" aside={<span className="ap-project-name">{projectId}</span>}>
      <p className="ap-note">
        A model asked about each tool call the classifier left open. Observing sends each such call
        to TypeSafe and changes nothing; enforcing lets its answer stand, except for the classes
        and guards it may refuse but never approve.
      </p>
      {status.isError && status.data === undefined && (
        <ErrorNote>the núcleo did not answer — nothing is known about the judge</ErrorNote>
      )}
      {current !== undefined && (
        <p className="ap-reason">
          The judge is <strong>{current}</strong> for {projectId}.
        </p>
      )}
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
            {readiness.reviewed} distinct actions reviewed, {readiness.agree} agreed — enforce needs{" "}
            {READINESS_MIN_REVIEWED} at {READINESS_MIN_AGREE_PERCENT}%, on an active project.
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
      <div className="ap-actions">
        {MODES.map(({ mode, label, confirm }) => (
          <ConfirmButton
            key={mode}
            label={label}
            confirmLabel={confirm}
            variant={mode === "enforce" ? "danger" : "ghost"}
            describedBy={mode === "enforce" ? "judge-residual-risk" : undefined}
            disabled={setJudge.isPending || current === mode || (mode === "enforce" && enforceShut)}
            onConfirm={() => setJudge.mutate({ project_id: projectId, judge: mode })}
          />
        ))}
      </div>
      {setJudge.isError && <JudgeRefusal error={setJudge.error} />}
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
  return (
    <Panel title="Judge verdicts" aside={<Count n={projectId === null ? undefined : rows.length} />}>
      {projectId === null && <Quiet says="choose a project above." />}
      {projectId !== null && verdicts.data !== undefined && rows.length === 0 && (
        <Quiet says="no verdict of the judge is waiting for you." />
      )}
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
