import { useState } from "react";
import { ApiRefusal } from "../data/client";
import { usePromoteAction, type ActionPair, type PromotionReport } from "../data/devtime";
import { percentText } from "../lib/format";
import { ConfirmButton, ErrorNote } from "../ui";

/**
 * The five promotion criteria of one `(action, rule)` pair and the Promote button, enabled only
 * when `report.ready`, behind the house confirmation. The daemon re-checks the criteria and
 * refuses with `not_ready` when they stopped holding between the read and the click.
 */

interface Criterion {
  key: string;
  label: string;
  met: boolean;
  figures: string;
}

/** Exactly the five booleans `report.ready` is the AND of, in the core's order. */
function criteria(report: PromotionReport): Criterion[] {
  return [
    {
      key: "eligible", label: "Rule eligible", met: report.action_eligible,
      figures: `Wilson lower bound ${report.wilson_lower.toFixed(2)}`,
    },
    {
      key: "sample", label: "Enough resolved cases", met: report.sample_ok,
      figures: `${report.n_resolved} resolved, ${report.n_sessions} sessions, ${report.days_covered} days`,
    },
    {
      key: "shadow", label: "Shadow approved", met: report.shadow_approved,
      figures: `hit rate ${percentText(report.hit_rate)}`,
    },
    {
      key: "regression", label: "No regression", met: report.regression_ok,
      figures: `avoidable ${percentText(report.avoidable_now)} now, ${percentText(report.avoidable_prev)} before`,
    },
    {
      key: "human", label: "Human review", met: report.human_ok,
      figures: `${report.human_marks} marks, useful rate ${percentText(report.human_rate)}`,
    },
  ];
}

export function ActionPromotionCard({ pair }: { pair: ActionPair }) {
  const promote = usePromoteAction();
  const [refused, setRefused] = useState<string | null>(null);
  const live = pair.mode === "live";
  const confirm = () => {
    setRefused(null);
    promote.mutate(
      { action: pair.action, rule: pair.rule_id },
      {
        onError: (error) =>
          setRefused(error instanceof ApiRefusal && error.code === "not_ready"
            ? "Not promoted: the criteria no longer hold."
            : "Not promoted: the daemon could not be reached."),
      },
    );
  };
  return (
    <div className="an-dt-promotion">
      <ul className="an-dt-criteria" aria-label="Promotion criteria">
        {criteria(pair.report).map((criterion) => (
          <li key={criterion.key}>
            <span>{criterion.label}</span>{" "}
            <span>{criterion.met ? "met" : "not met"}</span>{" "}
            <span className="an-note">{criterion.figures}</span>
          </li>
        ))}
      </ul>
      {live ? (
        <p className="an-note">{`Live since ${pair.since ?? "an unknown date"}${pair.changed_by === null ? "" : `, by ${pair.changed_by}`}`}</p>
      ) : (
        <ConfirmButton
          label="Promote"
          confirmLabel="Really promote"
          sayAs="Promote this action to live"
          variant="approve"
          disabled={!pair.report.ready || promote.isPending}
          onConfirm={confirm}
        />
      )}
      {refused === null ? null : <ErrorNote>{refused}</ErrorNote>}
    </div>
  );
}
