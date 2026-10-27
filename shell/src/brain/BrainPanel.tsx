import { DAEMON_NAME } from "../copy";
import { useMemo, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { ApiRefusal } from "../data/client";
import type { ChannelsReadout } from "../data/channels";
import { keys } from "../data/keys";
import { useKnowledge, type Known } from "../data/knowledge";
import {
  originLabel,
  TEACH_KINDS,
  useNotesGraph,
  useOwnerNote,
  useRemoveLink,
  useTeachNote,
  useUpdateNote,
  type NoteLink,
  type NotesGraph,
  type TargetKind,
  type TeachKind,
} from "../data/owner-notes";
import { useProjects } from "../data/system";
import { Button, ConfirmButton, ErrorNote, Quiet, RefusalNote, RelativeTime, Select, Skeleton } from "../ui";
import { ForceGraph } from "./ForceGraph";
import { Segments } from "./Segments";
import { backlinks, buildModel, localModel } from "./graph-model";
import type { GFilters, GModel, GNode } from "./graph-types";
import { EDGE_TYPES, NODE_KINDS } from "./GraphFilters";
import { itemOfNode, nodeIdOf, parseItem } from "./item-ref";
import { AddLinkForm } from "./LinkPicker";
import { NoteTextEdit } from "./NoteTextEdit";
import { NotesAbout } from "./NotesAbout";
import { RelatedNotes } from "./RelatedNotes";

/**
 * The side panel of the Brain graph: what the selected node is, and for a note, everything that
 * can be done to it. A note's text is shown as markdown, and can be edited in place.
 *
 * Backlinks and the local graph read the notes graph WITH archived notes (spec §3.2): a link from
 * an archived note still points here, and says so. The global graph's archived filter does not
 * reach this panel.
 */

export interface BrainPanelProps {
  /** `n:<id>`, `k:<id>` or `<kind>:<ref>`, as `buildModel` names them. */
  nodeId: string | null;
  /** The notes graph the global view drew from (its archived filter applied). */
  graph: NotesGraph;
  /** The global model, as filtered: what an entity's "Linked from" is read from. */
  model: GModel;
  /** A neighbour picked in the local graph or the backlinks: the same contract as a graph click. */
  onSelect?: (nodeId: string) => void;
}

const TEACH_SENTENCES: Record<string, string> = {
  already_taught: "This note is already taught. Its lesson is still pending or active.",
  archived: "An archived note cannot be taught: restore it first.",
  unknown_kind: `The ${DAEMON_NAME} does not know that kind of lesson.`,
};

/** Everything around a note, whatever the global filters hide: the local graph is the neighbourhood as it is. */
const LOCAL_FILTERS: GFilters = {
  state: "all",
  nodeKinds: new Set(NODE_KINDS),
  edgeTypes: new Set(EDGE_TYPES),
  showArchived: true,
  showOrphans: true,
};

const EMPTY_KNOWLEDGE: Known[] = [];

/** A note picked in a related/about list (`note:<id>`) goes through the panel's own `onSelect` as `n:<id>`. */
function selectItem(item: string, onSelect?: (nodeId: string) => void) {
  const ref = parseItem(item);
  const nodeId = ref === null ? null : nodeIdOf(ref);
  if (nodeId !== null) onSelect?.(nodeId);
}

/** A refusal as the daemon's own text; anything else means there was no answer. */
function Failure({ error, sentences }: { error: unknown; sentences?: Record<string, string> }) {
  if (error instanceof ApiRefusal) return <RefusalNote refusal={error} sentences={sentences} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding</ErrorNote>;
}

export function BrainPanel({ nodeId, graph, model, onSelect }: BrainPanelProps) {
  const wide = useNotesGraph(true);
  if (nodeId === null) return <Quiet says="Select a node to see what it is linked to." />;
  // The archived-inclusive graph decides whether a note exists; the global one stands in while it loads.
  const known = wide.data ?? graph;
  const item = itemOfNode(nodeId);
  if (item?.kind === "note" && known.notes.some((note) => note.id === item.id)) {
    // Keyed by the node, so a title typed or a "proposed" shown for one note never carries over
    // to the next one selected.
    return <NotePanel key={nodeId} id={item.id} graph={known} wide={wide.data} onSelect={onSelect} />;
  }
  const node = model.nodes.find((n) => n.id === nodeId);
  if (node !== undefined) return <OtherPanel node={node} model={model} onSelect={onSelect} />;
  // A well-formed note id the archived-inclusive graph does not know is gone, not merely filtered.
  if (item?.kind === "note" && wide.data !== undefined) {
    const gone: GNode = {
      id: nodeId,
      kind: "note",
      ref: String(item.id),
      label: `note ${item.id}`,
      bucket: "in_force",
      missing: true,
      degree: 0,
    };
    return <OtherPanel node={gone} model={model} onSelect={onSelect} />;
  }
  return <Quiet says="That node is not in the graph as filtered." />;
}

/** An entity, a knowledge row, or a note that is gone: a label and who points at it. */
function OtherPanel({
  node,
  model,
  onSelect,
}: {
  node: GNode;
  model: GModel;
  onSelect?: (nodeId: string) => void;
}) {
  const label = node.kind === "knowledge" || node.kind === "note" ? node.label : `${node.kind}: ${node.label}`;
  // A note stub that is not gone is one hidden because it is archived.
  const hidden = node.kind === "note" && !node.missing;
  const from = model.edges
    .filter((edge) => edge.target === node.id)
    .map((edge) => ({ edge, source: model.nodes.find((n) => n.id === edge.source) }));

  return (
    <div className="brain-panel">
      <h3>
        {label}
        {node.missing && <span className="brain-node-tag">gone</span>}
        {hidden && <span className="brain-node-tag">archived</span>}
      </h3>
      <section className="brain-sec">
        <h4>Linked from</h4>
        {from.length === 0 ? (
          <Quiet says="Nothing links here." />
        ) : (
          <ul aria-label="Linked from" className="brain-links">
            {from.map(({ edge, source }) => (
              <li key={edge.id}>
                <span className="brain-link-type">{edge.type}</span>
                <span className="brain-link-target">{source?.label ?? edge.source}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
      {/* A note stub (gone or hidden) is a note, not a target something is written about. */}
      {/* A theme is a derived cluster, not a target a note can be written about. */}
      {node.kind !== "note" && node.kind !== "theme" && (
        <NotesAbout
          kind={node.kind satisfies TargetKind}
          ref={node.ref}
          onItem={(item) => selectItem(item, onSelect)}
        />
      )}
    </div>
  );
}

function NotePanel({
  id,
  graph,
  wide,
  onSelect,
}: {
  id: number;
  graph: NotesGraph;
  /** The notes graph with archived notes, once it has answered. */
  wide: NotesGraph | undefined;
  onSelect?: (nodeId: string) => void;
}) {
  const detail = useOwnerNote(id);
  const update = useUpdateNote();
  const remove = useRemoveLink();
  // Accepted limitation: this reads the cache without subscribing or fetching (the notes panel must
  // not request /channels). If `channels.all` loads only after this paints, the label stays the
  // fallback ("Telegram") until another render; it does not repaint by itself.
  const cachedChannels = useQueryClient().getQueryData<ChannelsReadout>(keys.channels.all);

  if (detail.isError) return <ErrorNote>the {DAEMON_NAME} is not responding: the note is not known</ErrorNote>;
  if (detail.data === undefined) return <Skeleton label="Loading the note" lines={4} />;
  const { note, links_out, links_in, events } = detail.data;
  const archived = note.state === "archived";
  // Oldest first, whatever order the daemon sent.
  const history = [...events].sort((a, b) => a.at.localeCompare(b.at) || a.id - b.id);

  return (
    <div className="brain-panel">
      <header className="brain-panel-head">
        <NoteTextEdit id={id} text={note.text} disabled={archived} />
        <div className="brain-meta">
          <span className="brain-origin">{originLabel(note.origin, cachedChannels?.channels)}</span>
          <span className="brain-state">{note.state}</span>
          <RelativeTime at={note.created_at} />
          <span className="brain-meta-end">
            <Button
              variant="quiet"
              disabled={update.isPending}
              onClick={() => update.mutate({ id, state: archived ? "active" : "archived" })}
            >
              {archived ? "Restore" : "Archive"}
            </Button>
          </span>
        </div>
        {update.isError && <Failure error={update.error} />}
      </header>

      {!archived && <Teach id={id} />}

      <LocalGraph id={id} wide={wide} onSelect={onSelect} />

      <section className="brain-sec">
        <h4>Links out</h4>
        {links_out.length === 0 && <Quiet says="No links out." />}
        <ul aria-label="Links out" className="brain-links">
          {links_out.map((link) => (
            <li key={link.id}>
              <span className="brain-link-type">{link.link_type}</span>
              <span className="brain-link-target">
                <TargetName link={link} graph={graph} />
              </span>
              <span className="brain-link-act">
                <ConfirmButton
                  label="Remove"
                  confirmLabel="Remove link"
                  subject={`#${link.id}`}
                  variant="quiet"
                  disabled={remove.isPending}
                  onConfirm={() => remove.mutate(link.id)}
                />
              </span>
            </li>
          ))}
        </ul>
        {remove.isError && <Failure error={remove.error} />}
        <AddLinkForm id={id} />
      </section>

      <section className="brain-sec">
        <h4>Links in</h4>
        <LinksIn id={id} wide={wide} fallback={links_in} onSelect={onSelect} />
      </section>

      <RelatedNotes id={id} onItem={(item) => selectItem(item, onSelect)} />

      <section className="brain-sec">
        <h4>History</h4>
        <ol aria-label="History" className="brain-history">
          {history.map((event) => (
            <li key={event.id}>
              <span className="brain-history-kind">
                {event.kind}
                {event.detail !== null && event.detail !== "" ? `: ${event.detail}` : ""}
              </span>{" "}
              <RelativeTime at={event.at} />
            </li>
          ))}
        </ol>
      </section>
    </div>
  );
}

/**
 * Every link pointing at this note (its backlinks), archived sources included and marked. Until
 * the archived-inclusive graph answers, the note's own `links_in` stands in.
 */
function LinksIn({
  id,
  wide,
  fallback,
  onSelect,
}: {
  id: number;
  wide: NotesGraph | undefined;
  fallback: NoteLink[];
  onSelect?: (nodeId: string) => void;
}) {
  const links = wide === undefined ? fallback : backlinks(wide, "note", String(id));
  if (links.length === 0) return <Quiet says="No links in." />;
  return (
    <ul aria-label="Links in" className="brain-links">
      {links.map((link) => {
        const source = wide?.notes.find((n) => n.id === link.note_id);
        const name = source !== undefined ? firstLine(source.text) : `note ${link.note_id}`;
        return (
          <li key={link.id}>
            <span className="brain-link-type">{link.link_type}</span>
            <span className="brain-link-target">
              {onSelect !== undefined ? (
                <Button variant="quiet" onClick={() => onSelect(`n:${link.note_id}`)}>
                  {name}
                </Button>
              ) : (
                name
              )}
              {source?.state === "archived" && <span className="brain-node-tag">archived</span>}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** First line of a note, trimmed: a note has no title of its own. */
function firstLine(text: string): string {
  const first = text.trim().split("\n")[0] ?? "";
  return first.length > 60 ? `${first.slice(0, 59)}…` : first || "(empty note)";
}

const DEPTHS: readonly (readonly [1 | 2, string])[] = [
  [1, "Depth 1"],
  [2, "Depth 2"],
];

/** The note's neighbourhood at depth 1 (or 2), drawn by the same canvas as the global graph. */
function LocalGraph({
  id,
  wide,
  onSelect,
}: {
  id: number;
  wide: NotesGraph | undefined;
  onSelect?: (nodeId: string) => void;
}) {
  const [depth, setDepth] = useState<1 | 2>(1);
  const knowledge = useKnowledge();
  const projects = useProjects();
  const self = `n:${id}`;
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
        <Segments label="Local graph depth" value={depth} options={DEPTHS} onChange={setDepth} />
      </div>
      {local === null ? (
        <Skeleton label="Loading the local graph" lines={3} />
      ) : (
        <ForceGraph
          compact
          model={local}
          selected={self}
          onSelect={(nodeId) => {
            if (nodeId !== self) onSelect?.(nodeId);
          }}
        />
      )}
    </section>
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

/** Turns an active note into a pending lesson; the owner answers it in Learned. */
function Teach({ id }: { id: number }) {
  const teach = useTeachNote();
  const [kind, setKind] = useState<TeachKind>("memory");
  const [title, setTitle] = useState("");

  return (
    <section className="brain-sec" role="group" aria-label="Teach the agent">
      <h4>Make it a lesson</h4>
      <div className="brain-panel-form">
      <Select
        density="sm"
        mono
        aria-label="Lesson kind"
        value={kind}
        onChange={(event) => setKind(event.target.value as TeachKind)}
      >
        {TEACH_KINDS.map((value) => (
          <option key={value}>{value}</option>
        ))}
      </Select>
      <input
        aria-label="Lesson title"
        className="brain-prose-input"
        placeholder="Title (optional)"
        value={title}
        onChange={(event) => setTitle(event.target.value)}
      />
      <Button
        variant="ghost"
        disabled={teach.isPending}
        onClick={() =>
          teach.mutate({ id, kind, ...(title.trim() !== "" ? { title: title.trim() } : {}) })
        }
      >
        Teach the agent
      </Button>
      </div>
      {teach.isSuccess && (
        <p role="status">
          Proposed. Waiting for your approval in <Link to="/brain" search={{ item: `knowledge:${teach.data.knowledge_id}` }}>the Brain</Link>
        </p>
      )}
      {teach.isError && <Failure error={teach.error} sentences={TEACH_SENTENCES} />}
    </section>
  );
}
