import { useId, useState } from "react";
import {
  useDevtimeActions,
  useDevtimeShadow,
  useMarkShadow,
  type ActionPair,
  type ShadowItem,
  type ShadowMark,
} from "../data/devtime";
import { Button, ErrorNote, Skeleton } from "../ui";
import { ActionPromotionCard } from "./ActionPromotionCard";
import { Card, unreachable } from "./sections";

/**
 * What the automatic actions would have said (shadow mode), pair by pair, with the owner's
 * `useful` / `noise` marks and a counter of how many of the five promotion criteria hold. Its own
 * queries, so its failure never takes the development time overview down with it.
 */

const MARKS: ShadowMark[] = ["useful", "noise"];
const safeId = (id: string) => id.replace(/[^a-zA-Z0-9_-]/g, "");

/** How many of the five criteria hold: `ready` is the AND of exactly these booleans. */
function criteriaMet(pair: ActionPair): number {
  const { report } = pair;
  return [report.action_eligible, report.sample_ok, report.shadow_approved, report.regression_ok, report.human_ok].filter(Boolean).length;
}

function paramsText(params: Record<string, unknown>): string {
  return Object.entries(params)
    .map(([key, value]) => `${key}: ${typeof value === "object" && value !== null ? JSON.stringify(value) : String(value)}`)
    .join(", ");
}

export function ActionsShadowPanel() {
  const pairs = useDevtimeActions();
  return (
    <Card label="Automatic actions" className="an-dt-actions">
      {pairs.data === undefined ? (
        pairs.isError ? (
          <ErrorNote onRetry={() => void pairs.refetch()} retryLabel="Try again">
            {unreachable("automatic actions")}
          </ErrorNote>
        ) : (
          <Skeleton label="Loading automatic actions" lines={3} />
        )
      ) : pairs.data.pairs.length === 0 ? (
        <p className="an-note">No automatic actions are configured.</p>
      ) : (
        pairs.data.pairs.map((pair) => <PairBlock key={`${pair.action}:${pair.rule_id}`} pair={pair} />)
      )}
    </Card>
  );
}

function PairBlock({ pair }: { pair: ActionPair }) {
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const shadow = useDevtimeShadow({ action: pair.action, rule: pair.rule_id, cursor });
  const titleId = `an-dt-pair-${safeId(useId())}`;
  return (
    <section className="an-dt-pair" aria-labelledby={titleId}>
      <h3 id={titleId} className="an-dt-share-title">{`${pair.action} · ${pair.rule_id}`}</h3>
      <p className="an-note">
        <span>{pair.mode}</span>{" · "}
        <span>{`${criteriaMet(pair)}/5`}</span> <span>criteria met</span>
      </p>
      <ActionPromotionCard pair={pair} />
      {shadow.data === undefined ? (
        shadow.isError ? (
          <ErrorNote onRetry={() => void shadow.refetch()} retryLabel="Try again">
            {unreachable("shadow decisions")}
          </ErrorNote>
        ) : (
          <Skeleton label="Loading shadow decisions" lines={2} />
        )
      ) : (
        <>
          <ul className="an-dt-finding-list">
            {shadow.data.items.map((item) => (
              <ShadowRow key={item.id} item={item} />
            ))}
          </ul>
          {shadow.data.items.length === 0 ? <p className="an-note">No shadow decisions yet.</p> : null}
          {shadow.data.next_cursor !== null ? (
            <Button onClick={() => setCursor(shadow.data?.next_cursor ?? undefined)}>Next page</Button>
          ) : null}
        </>
      )}
    </section>
  );
}

/** Optimistic like a finding's mark: the pressed button flips at once and reverts if the POST fails. */
function ShadowRow({ item }: { item: ShadowItem }) {
  const mark = useMarkShadow();
  const [shown, setShown] = useState<ShadowMark | null>(item.human_verdict);
  const [failed, setFailed] = useState(false);
  const params = paramsText(item.params);
  const choose = (verdict: ShadowMark) => {
    const before = shown;
    setShown(verdict);
    setFailed(false);
    mark.mutate(
      { id: item.id, verdict },
      {
        onSuccess: (out) => setShown(out.human_verdict),
        onError: () => { setShown(before); setFailed(true); },
      },
    );
  };
  return (
    <li className="an-dt-finding">
      <span>{item.template_id}</span>
      {params === "" ? null : <span>{params}</span>}
      <span>{item.decided_at}</span>
      <span>{item.decision === "suppress" ? `suppressed: ${item.suppress_reason ?? "no reason given"}` : "would fire"}</span>
      <span>{item.outcome ?? item.outcome_state}</span>
      {item.decision === "fire"
        ? MARKS.map((verdict) => (
            <Button key={verdict} variant="quiet" aria-pressed={shown === verdict} onClick={() => choose(verdict)}>
              {verdict}
            </Button>
          ))
        : null}
      {failed ? <ErrorNote>The mark was not saved.</ErrorNote> : null}
    </li>
  );
}
