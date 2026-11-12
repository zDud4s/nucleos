import { Handle, Position, useStore, type Node, type NodeProps } from "@xyflow/react";
import { usd } from "../ui";
import { useNow } from "../lib/useNow";
import { elapsed, STATE_TEXT, tokens } from "./format";
import { useNodeEntry } from "./hooks";
import { identityOf, useParentHarness } from "./identity";
import { NodeActions } from "./NodeActions";
import { TermBody } from "./TermBody";
import { useFleetUi } from "./ui-context";
import { selectCard } from "./zoom";

export type FlowAgent = Node<{ id: string }, "agentTerm">;

/**
 * The `agentTerm` node. It reads its own record, lines and metrics from the store, so an event for
 * another node never renders this one, and `data` carries only the id (a stable object while the
 * layout is unchanged).
 */
export function FleetNode({ data }: NodeProps<FlowAgent>) {
  const ui = useFleetUi();
  const entry = useNodeEntry(data.id);
  const parentHarness = useParentHarness(entry?.node.parent_id ?? null);
  const card = useStore(selectCard);
  const ticking = entry !== undefined && (entry.node.state === "busy" || entry.node.state === "idle");
  const now = useNow(1000, ticking);
  if (entry === undefined) return null;

  const { node, lines, metrics } = entry;
  const identity = identityOf(node, parentHarness);
  const end = node.ended_at !== null ? Date.parse(node.ended_at) : ticking ? now : metrics === null ? null : Date.parse(metrics.last_activity_at);
  const classes = [
    "fleet-node",
    `fleet-node-${node.state}`,
    card ? "fleet-node-card" : "",
    ui.path.has(node.id) ? "fleet-node-path" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <article
      className={classes}
      aria-label={`${node.label}, ${STATE_TEXT[node.state]}`}
      tabIndex={0}
      onClick={() => ui.select(node.id)}
      onKeyDown={(event) => {
        if ((event.key === "Enter" || event.key === " ") && event.target === event.currentTarget) {
          event.preventDefault();
          ui.select(node.id);
        }
      }}
    >
      <Handle type="target" position={Position.Top} isConnectable={false} className="fleet-handle" />
      <header className="fleet-hdr">
        <span className="fleet-dot" aria-hidden="true" />
        <span className="fleet-label" title={node.label}>
          {node.label}
        </span>
        {node.role !== null && <span className="fleet-role">{node.role}</span>}
        <span className="fleet-grow" />
        <span className={identity.crossHarness ? "fleet-hm fleet-hm-cross" : "fleet-hm"}>{identity.text}</span>
      </header>
      <TermBody lines={lines} label={node.label} card={card} />
      <footer className="fleet-ftr">
        <span className={`fleet-st fleet-st-${node.state}`}>{STATE_TEXT[node.state]}</span>
        <span className="fleet-sep" aria-hidden="true">·</span>
        <span>{end === null ? "—" : elapsed(end - Date.parse(node.started_at))}</span>
        <span className="fleet-sep" aria-hidden="true">·</span>
        <span>ctx {tokens(metrics?.context_tokens ?? null)}</span>
        <span className="fleet-sep" aria-hidden="true">·</span>
        <span>{metrics?.cost_usd == null ? "—" : usd(metrics.cost_usd)}</span>
        <span className="fleet-grow" />
        <NodeActions node={node} />
      </footer>
      <Handle type="source" position={Position.Bottom} isConnectable={false} className="fleet-handle" />
    </article>
  );
}
