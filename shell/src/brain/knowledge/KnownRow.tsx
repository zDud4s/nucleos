import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  distilledOrigin,
  parseEvidence,
  useDistillCauses,
  useKnowledge,
  useNearDuplicates,
  type Known,
} from "../../data/knowledge";
import { Button, RelativeTime, Row, StateBadge } from "../../ui";
import { Chain } from "./Chain";
import "./knowledge.css";

export interface KnownRowProps {
  row: Known;
  /** Whatever can be done to the row; a property of the list it is in. */
  decisions?: ReactNode;
}

/**
 * One thing known, with whatever can be done to it.
 *
 * `decisions` is a slot rather than a status check inside the row: what a person
 * may do to a row is a property of the list it is in — you approve what
 * is waiting, revert what is in force, and do nothing at all to what is over.
 */
export function KnownRow({ row, decisions }: KnownRowProps) {
  const [chainId, setChainId] = useState<number | null>(null);
  const evidence = parseEvidence(row.evidence);
  // One cached query however many rows ask; only a distiller row reads its answer.
  const causes = useDistillCauses();
  const origin = distilledOrigin(row, causes.data ?? new Map());
  // Same: shared cached queries. The title of the older row comes from the list already loaded,
  // and only a near-duplicate asks for it, so a row shown under one scope never reads them all.
  const duplicateOf = useNearDuplicates().data?.get(row.id) ?? null;
  const all = useKnowledge(duplicateOf !== null).data;
  const duplicateTitle =
    duplicateOf === null ? null : (all?.find((other) => other.id === duplicateOf)?.title ?? null);

  return (
    <Row className="learned-row">
      <div className="learned-head">
        <StateBadge domain="knowledge" state={row.kind} />
        <span className="learned-scope">
          {row.scope_id ?? "this machine"}
        </span>
        <span className="learned-layer">{row.layer}</span>
        <span className="learned-source">{row.source}</span>
        {row.observations !== null && (
          <span className="learned-count">
            measured {row.observations} time{row.observations === 1 ? "" : "s"}
          </span>
        )}
        <span className="learned-when">
          <RelativeTime at={row.activated_at ?? row.created_at} />
        </span>
      </div>

      <p className="learned-title">{row.title}</p>
      {origin !== null && (
        <p className="learned-origin">
          distilled from{" "}
          {origin.job === null ? (
            "a job"
          ) : (
            <Link to="/fleet">job #{origin.job}</Link>
          )}
          {origin.causeLabel !== null && <> · {origin.causeLabel}</>}
        </p>
      )}
      {duplicateOf !== null && (
        <p className="learned-duplicate">
          <span className="learned-duplicate-mark">possible duplicate</span> of{" "}
          <Button
            variant="quiet"
            aria-expanded={chainId === duplicateOf}
            onClick={() => setChainId(chainId === duplicateOf ? null : duplicateOf)}
          >
            #{duplicateOf}
          </Button>
          {duplicateTitle !== null && <> {duplicateTitle}</>}
        </p>
      )}
      <p className="learned-body">{row.body}</p>

      {evidence.length > 0 && (
        <ul className="learned-evidence">
          {evidence.map((ref, index) => (
            <li key={`${ref.t}-${String(ref.id)}-${index}`}>
              {ref.t === "run" ? (
                <Link to="/runs/$runId" params={{ runId: String(ref.id) }}>
                  run {ref.id}
                </Link>
              ) : ref.t === "job" ? (
                <Link to="/fleet">job #{ref.id}</Link>
              ) : ref.t === "knowledge" && knowledgeId(ref.id) !== null ? (
                <Button
                  variant="quiet"
                  aria-expanded={chainId === knowledgeId(ref.id)}
                  onClick={() => {
                    const id = knowledgeId(ref.id);
                    if (id !== null) setChainId(chainId === id ? null : id);
                  }}
                >
                  knowledge {ref.id}
                </Button>
              ) : (
                <span>
                  {ref.t} {ref.id}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="learned-foot">
        <Button
          variant="quiet"
          aria-expanded={chainId === row.id}
          onClick={() => setChainId(chainId === row.id ? null : row.id)}
        >
          {row.supersedes === null ? "History" : "What it replaced"}
        </Button>
        {decisions}
      </div>

      {chainId !== null && <Chain id={chainId} />}
    </Row>
  );
}

function knowledgeId(id: number | string): number | null {
  const numeric = typeof id === "number" ? id : Number(id);
  return Number.isFinite(numeric) ? numeric : null;
}
