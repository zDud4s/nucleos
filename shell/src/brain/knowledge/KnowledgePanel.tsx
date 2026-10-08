import { useMemo, useState } from "react";
import {
  useApproveKnowledge,
  useKnowledge,
  useRejectKnowledge,
  useRevertKnowledge,
  type Known,
} from "../../data/knowledge";
import { useNotesGraph, type NotesGraph } from "../../data/owner-notes";
import { useProjects } from "../../data/system";
import { Button, ConfirmButton, ErrorNote, Quiet } from "../../ui";
import { ForceGraph } from "../ForceGraph";
import { Segments } from "../Segments";
import { backlinks, buildModel, localModel } from "../graph-model";
import type { GFilters } from "../graph-types";
import { EDGE_TYPES, NODE_KINDS } from "../GraphFilters";
import { KnownRow } from "./KnownRow";
import { DecisionRefusal } from "./WaitingPanel";

export interface KnowledgePanelProps {
  /** The knowledge row's id. */
  id: number;
  /** A neighbour picked in the local graph or the backlinks: a node id (`n:12`, `k:5`, `project:foo`). */
  onSelect(ref: string): void;
}

/** Everything around the row, whatever the global filters hide: the local graph is the neighbourhood as it is. */
const LOCAL_FILTERS: GFilters = {
  state: "all",
  nodeKinds: new Set(NODE_KINDS),
  edgeTypes: new Set(EDGE_TYPES),
  showArchived: true,
  showOrphans: true,
};

const EMPTY_KNOWLEDGE: Known[] = [];

/** First line of a note, trimmed: a note has no title of its own. */
function firstLine(text: string): string {
  const first = text.trim().split("\n")[0] ?? "";
  return first.length > 60 ? `${first.slice(0, 59)}…` : first || "(empty note)";
}

/**
 * The side panel of a knowledge row in the unified Brain view: the row as Learned shows it (with
 * its decisions), the notes pointing at it, its neighbourhood, and a reserved place for where it
 * was used. Backlinks and the local graph read the archived-inclusive notes graph.
 */
export function KnowledgePanel({ id, onSelect }: KnowledgePanelProps) {
  const knowledge = useKnowledge();
  const wide = useNotesGraph(true);
  const approve = useApproveKnowledge();
  const reject = useRejectKnowledge();
  const revert = useRevertKnowledge();

  if (knowledge.isError) return <ErrorNote>the núcleo did not answer — the row is not known</ErrorNote>;
  if (knowledge.data === undefined) return <Quiet says="Loading…" />;
  const row = knowledge.data.find((candidate) => candidate.id === id);
  if (row === undefined) {
    return (
      <div className="brain-panel">
        <h3>
          {`knowledge ${id}`}
          <span className="brain-node-tag">gone</span>
        </h3>
        <Quiet says="This row is no longer in the store." />
      </div>
    );
  }

  const deciding = approve.isPending || reject.isPending || revert.isPending;
  const refusal = approve.error ?? reject.error ?? revert.error;
  const proposalId = row.proposal_id;

  let decisions = null;
  if (row.status === "proposed" && proposalId !== null) {
    decisions = (
      <>
        <Button variant="approve" disabled={deciding} onClick={() => approve.mutate(proposalId)}>
          Approve
        </Button>
        <Button disabled={deciding} onClick={() => reject.mutate(proposalId)}>
          Refuse
        </Button>
      </>
    );
  } else if (row.status === "active") {
    decisions = (
      <ConfirmButton
        label="Revert"
        confirmLabel="It no longer applies"
        variant="quiet"
        disabled={deciding}
        onConfirm={() => revert.mutate(row.id)}
      />
    );
  }

  return (
    <div className="brain-panel">
      <div className="brain-meta">
        <span className="brain-state">{row.status}</span>
      </div>
      {refusal !== null && <DecisionRefusal error={refusal} />}
      <KnownRow row={row} decisions={decisions} />

      <section className="brain-sec">
        <h4>Notes pointing here</h4>
        <Backlinks id={id} wide={wide.data} onSelect={onSelect} />
      </section>

      <LocalGraph id={id} wide={wide.data} onSelect={onSelect} />

      <section className="brain-sec">
        <h4>Used in prompts</h4>
        <Quiet says="Not tracked here yet." />
      </section>
    </div>
  );
}

function Backlinks({
  id,
  wide,
  onSelect,
}: {
  id: number;
  wide: NotesGraph | undefined;
  onSelect(ref: string): void;
}) {
  if (wide === undefined) return <Quiet says="Loading…" />;
  const links = backlinks(wide, "knowledge", String(id));
  if (links.length === 0) return <Quiet says="No note points here." />;
  return (
    <ul aria-label="Notes pointing here" className="brain-links">
      {links.map((link) => {
        const source = wide.notes.find((note) => note.id === link.note_id);
        const name = source !== undefined ? firstLine(source.text) : `note ${link.note_id}`;
        return (
          <li key={link.id}>
            <span className="brain-link-type">{link.link_type}</span>
            <span className="brain-link-target">
              <Button variant="quiet" onClick={() => onSelect(`n:${link.note_id}`)}>
                {name}
              </Button>
              {source?.state === "archived" && <span className="brain-node-tag">archived</span>}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** The row's neighbourhood at depth 1 (or 2), drawn by the same canvas as the global graph. */
function LocalGraph({
  id,
  wide,
  onSelect,
}: {
  id: number;
  wide: NotesGraph | undefined;
  onSelect(ref: string): void;
}) {
  const [depth, setDepth] = useState<1 | 2>(1);
  const knowledge = useKnowledge();
  const projects = useProjects();
  const self = `k:${id}`;
  const knownRows = knowledge.data ?? EMPTY_KNOWLEDGE;
  const projectRows = projects.data;
  const local = useMemo(
    () =>
      wide === undefined ? null : localModel(buildModel(wide, knownRows, projectRows, LOCAL_FILTERS), self, depth),
    [wide, knownRows, projectRows, self, depth],
  );

  return (
    <section className="brain-sec brain-local" aria-label="Local graph">
      <div className="brain-local-head">
        <h4>Local graph</h4>
        <Segments
          label="Local graph depth"
          value={depth}
          options={[
            [1, "Depth 1"],
            [2, "Depth 2"],
          ]}
          onChange={setDepth}
        />
      </div>
      {local === null ? (
        <Quiet says="Loading the local graph…" />
      ) : (
        <ForceGraph
          compact
          model={local}
          selected={self}
          onSelect={(nodeId) => {
            if (nodeId !== self) onSelect(nodeId);
          }}
        />
      )}
    </section>
  );
}
