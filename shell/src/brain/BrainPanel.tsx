import { useState } from "react";
import { Link } from "@tanstack/react-router";
import type { Edge, Node } from "@xyflow/react";
import { ApiRefusal } from "../data/client";
import {
  LINK_TYPES,
  TARGET_KINDS,
  TEACH_KINDS,
  useAddLink,
  useOwnerNote,
  useRemoveLink,
  useTeachNote,
  useUpdateNote,
  type LinkType,
  type NoteLink,
  type NotesGraph,
  type TargetKind,
  type TeachKind,
} from "../data/owner-notes";
import { Button, ConfirmButton, ErrorNote, Quiet, RefusalNote, RelativeTime } from "../ui";

/**
 * The side panel of the Brain graph: what the selected node is, and for a note, everything that
 * can be done to it. A note's own text is shown as written; nothing here edits it.
 */

export interface BrainPanelProps {
  /** `n:<id>`, `k:<id>` or `<kind>:<ref>`, as `toGraph` names them. */
  nodeId: string | null;
  graph: NotesGraph;
  nodes: Node[];
  edges: Edge[];
}

const TEACH_SENTENCES: Record<string, string> = {
  already_taught: "This note is already taught — its lesson is still pending or active.",
  archived: "An archived note cannot be taught — restore it first.",
  unknown_kind: "The núcleo does not know that kind of lesson.",
};

/** A refusal as the daemon's own text; anything else means there was no answer. */
function Failure({ error, sentences }: { error: unknown; sentences?: Record<string, string> }) {
  if (error instanceof ApiRefusal) return <RefusalNote refusal={error} sentences={sentences} />;
  return <ErrorNote>the núcleo did not answer</ErrorNote>;
}

export function BrainPanel({ nodeId, graph, nodes, edges }: BrainPanelProps) {
  if (nodeId === null) return <Quiet says="Select a node to see what it is linked to." />;
  const node = nodes.find((n) => n.id === nodeId);
  if (node === undefined) return <Quiet says="That node is no longer in the graph." />;
  if (node.type === "note" && nodeId.startsWith("n:") && node.data.note !== null) {
    return <NotePanel id={Number(nodeId.slice(2))} graph={graph} />;
  }
  return <OtherPanel node={node} nodes={nodes} edges={edges} />;
}

/** An entity, a knowledge row, or a note that is gone: a label and who points at it. */
function OtherPanel({ node, nodes, edges }: { node: Node; nodes: Node[]; edges: Edge[] }) {
  const known = node.data.known as { title: string } | null | undefined;
  const note = node.data.note as { text: string } | null | undefined;
  const label =
    node.type === "knowledge"
      ? (known?.title ?? "knowledge")
      : node.type === "note"
        ? "note"
        : `${String(node.data.kind)}: ${String(node.data.label)}`;
  const missing = node.data.missing === true || (node.type === "note" && note === null);
  const from = edges
    .filter((edge) => edge.target === node.id)
    .map((edge) => ({
      edge,
      note: nodes.find((n) => n.id === edge.source)?.data.note as { text: string } | null,
    }));

  return (
    <div className="brain-panel">
      <h3>
        {label}
        {missing && <span className="brain-node-tag">gone</span>}
      </h3>
      <h4>Linked from</h4>
      {from.length === 0 ? (
        <Quiet says="Nothing links here." />
      ) : (
        <ul aria-label="Linked from">
          {from.map(({ edge, note: source }) => (
            <li key={edge.id}>
              {source?.text ?? edge.source} ({String(edge.label)})
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function NotePanel({ id, graph }: { id: number; graph: NotesGraph }) {
  const detail = useOwnerNote(id);
  const update = useUpdateNote();
  const remove = useRemoveLink();

  if (detail.isError) return <ErrorNote>the núcleo did not answer — the note is not known</ErrorNote>;
  if (detail.data === undefined) return <Quiet says="Loading…" />;
  const { note, links_out, links_in, events } = detail.data;
  const archived = note.state === "archived";
  // Oldest first, whatever order the daemon sent.
  const history = [...events].sort((a, b) => a.at.localeCompare(b.at) || a.id - b.id);

  return (
    <div className="brain-panel">
      <p className="brain-text">{note.text}</p>
      <div className="brain-meta">
        <span className="brain-origin">{note.origin}</span>
        <span className="brain-state">{note.state}</span>
        <RelativeTime at={note.created_at} />
      </div>
      <Button
        variant="quiet"
        disabled={update.isPending}
        onClick={() => update.mutate({ id, state: archived ? "active" : "archived" })}
      >
        {archived ? "Restore" : "Archive"}
      </Button>
      {update.isError && <Failure error={update.error} />}

      {!archived && <Teach id={id} />}

      <h4>History</h4>
      <ol aria-label="History">
        {history.map((event) => (
          <li key={event.id}>
            {event.kind}
            {event.detail !== null && event.detail !== "" ? ` — ${event.detail}` : ""}{" "}
            <RelativeTime at={event.at} />
          </li>
        ))}
      </ol>

      <h4>Links out</h4>
      {links_out.length === 0 && <Quiet says="No links out." />}
      <ul aria-label="Links out">
        {links_out.map((link) => (
          <li key={link.id}>
            {link.link_type} → <TargetName link={link} graph={graph} />{" "}
            <ConfirmButton
              label="Remove"
              confirmLabel="Remove link"
              subject={`#${link.id}`}
              variant="quiet"
              disabled={remove.isPending}
              onConfirm={() => remove.mutate(link.id)}
            />
          </li>
        ))}
      </ul>
      {remove.isError && <Failure error={remove.error} />}

      <h4>Links in</h4>
      {links_in.length === 0 && <Quiet says="No links in." />}
      <ul aria-label="Links in">
        {links_in.map((link) => (
          <li key={link.id}>
            {link.link_type} ← note {link.note_id}
          </li>
        ))}
      </ul>

      <AddLink id={id} />
    </div>
  );
}

function TargetName({ link, graph }: { link: NoteLink; graph: NotesGraph }) {
  const target = graph.targets.find((t) => t.kind === link.target_kind && t.ref === link.target_ref);
  return (
    <span>
      {link.target_kind}: {target?.label ?? link.target_ref}
      {target?.missing === true && <span className="brain-node-tag">gone</span>}
    </span>
  );
}

function AddLink({ id }: { id: number }) {
  const add = useAddLink();
  const [type, setType] = useState<LinkType>("relates");
  const [kind, setKind] = useState<TargetKind>("note");
  const [ref, setRef] = useState("");
  const blank = ref.trim() === "";

  return (
    <form
      className="brain-panel-form"
      aria-label="Add link"
      onSubmit={(event) => {
        event.preventDefault();
        if (blank || add.isPending) return;
        add.mutate(
          { noteId: id, link_type: type, target_kind: kind, target_ref: ref.trim() },
          { onSuccess: () => setRef("") },
        );
      }}
    >
      <select
        aria-label="Link type"
        value={type}
        onChange={(event) => setType(event.target.value as LinkType)}
      >
        {LINK_TYPES.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </select>
      <select
        aria-label="Target kind"
        value={kind}
        onChange={(event) => setKind(event.target.value as TargetKind)}
      >
        {TARGET_KINDS.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </select>
      <input
        aria-label="Target ref"
        placeholder="id or path"
        value={ref}
        onChange={(event) => setRef(event.target.value)}
      />
      <Button type="submit" variant="quiet" disabled={blank || add.isPending}>
        Add link
      </Button>
      {add.isError && <Failure error={add.error} />}
    </form>
  );
}

/** Turns an active note into a pending lesson; the owner answers it in Learned. */
function Teach({ id }: { id: number }) {
  const teach = useTeachNote();
  const [kind, setKind] = useState<TeachKind>("memory");
  const [title, setTitle] = useState("");

  return (
    <div className="brain-panel-form" role="group" aria-label="Teach the agent">
      <select
        aria-label="Lesson kind"
        value={kind}
        onChange={(event) => setKind(event.target.value as TeachKind)}
      >
        {TEACH_KINDS.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </select>
      <input
        aria-label="Lesson title"
        placeholder="title (optional)"
        value={title}
        onChange={(event) => setTitle(event.target.value)}
      />
      <Button
        variant="quiet"
        disabled={teach.isPending}
        onClick={() =>
          teach.mutate({ id, kind, ...(title.trim() !== "" ? { title: title.trim() } : {}) })
        }
      >
        Teach the agent
      </Button>
      {teach.isSuccess && (
        <p role="status">
          Proposed — waiting for your approval in <Link to="/learned">Learned</Link>
        </p>
      )}
      {teach.isError && <Failure error={teach.error} sentences={TEACH_SENTENCES} />}
    </div>
  );
}
