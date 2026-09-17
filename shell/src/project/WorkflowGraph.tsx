// §spec motor-de-workflows
import { useState } from "react";
import { WorkflowCanvas } from "../canvas/WorkflowCanvas";
import { inSequence, nodeMeaning, nodeTone } from "../canvas/workflow-model";
import { isApiRefusal } from "../data/client";
import {
  useSetNodeOverlay,
  useWorkflowGraph,
  type GraphNode,
  type WorkflowGraph as Graph,
} from "../data/workflow-graph";
import { openInVscode } from "../lib/vscode";
import { Button, Quiet, RefusalNote } from "../ui";

/**
 * The canvas, its inspector, and the guard that stands between an overlay and an eject.
 *
 * **Two different edits, and telling them apart is the whole of §6.2 and §6.3.** Which model runs a
 * node, which tool, and whether it runs here at all are the *overlay* — this project's to change,
 * with no copy of anything taken, which is what an overlay is for. Changing what the node actually
 * says — its instructions file — is changing the bundle, and that is where the three exits belong.
 *
 * Reading the second as the first is how every project ends up ejected: somebody wants a cheaper
 * model on one node, is offered "eject and edit", takes it, and stops receiving updates forever
 * over a one-word change.
 */

export interface WorkflowGraphProps {
  projectId: string;
  name: string;
  /** Where the bundle is on this machine, for §6.3's second exit. `null` when it is not here. */
  originPath: string | null;
  ejected: boolean;
}

export function WorkflowGraph({ projectId, name, originPath, ejected }: WorkflowGraphProps) {
  const graph = useWorkflowGraph(projectId, name);
  const [selected, setSelected] = useState<string | null>(null);

  if (graph.isError) return <GraphMissing error={graph.error} />;
  if (graph.data === undefined) {
    return <p className="text-xs text-text-faint">Reading the graph…</p>;
  }
  if (graph.data.nodes.length === 0) {
    /*
      The one-line absence, in the shared primitive rather than in a paragraph this file sizes and
      colours itself. There is nothing to teach here and nothing to press: the bundle is installed,
      it simply has no order declared in it yet, and `Teach` would answer that with a poster.
    */
    return (
      <Quiet says="This bundle has no sequence in it yet — skills and scripts and nothing saying in what order." />
    );
  }

  const node = graph.data.nodes.find((row) => row.id === selected) ?? null;

  return (
    <div className="flex flex-col gap-3">
      <Orphaned graph={graph.data} />
      <div className="flex flex-col gap-3 lg:flex-row">
        <div className="min-w-0 flex-1">
          <WorkflowCanvas
            nodes={graph.data.nodes}
            edges={graph.data.edges}
            selected={selected}
            onSelect={setSelected}
          />
        </div>
        <Inspector
          projectId={projectId}
          name={name}
          node={node}
          source={graph.data.source}
          originPath={originPath}
          ejected={ejected}
        />
      </div>
    </div>
  );
}

/**
 * Why there is no picture, in the daemon's words.
 *
 * A bundle with no graph and a graph that does not parse are different facts and get different
 * sentences: one sends somebody to write a file, the other to fix one, and a shared "could not draw
 * the workflow" would send them to the wrong place half the time.
 */
function GraphMissing({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <p className="text-xs text-text-muted">The núcleo did not answer about this graph.</p>;
  }
  if (error.code === "no_graph") {
    return (
      <p className="max-w-prose text-xs text-text-muted">
        This bundle has no <span className="font-mono">graph.yaml</span> in it. Skills and scripts
        with no sequence yet is a halfway state, not a broken bundle.
      </p>
    );
  }
  if (error.code === "invalid_graph") {
    return (
      <div className="max-w-prose rounded-md border border-tone-paused-border bg-tone-paused-bg p-3">
        <p className="text-xs text-text">
          <span className="font-mono">graph.yaml</span> does not parse, so nothing here can be drawn
          honestly.
        </p>
        <p className="mt-1 text-xs text-text-muted">{error.detail}</p>
      </div>
    );
  }
  return <p className="text-xs text-text-muted">{error.detail}</p>;
}

/**
 * Overrides applying to nothing.
 *
 * §12 asks for `switched off here` ≠ `not in the bundle`. The first is dotted on the canvas; the
 * second cannot be drawn at all, so it is said here — and this is the only moment somebody learns
 * that a bundle they updated dropped the node they had configured.
 */
function Orphaned({ graph }: { graph: Graph }) {
  if (graph.orphaned.length === 0) return null;
  return (
    <p className="rounded-md border border-tone-paused-border bg-tone-paused-bg p-2 text-xs text-text-muted">
      This project overrides {graph.orphaned.join(", ")}, which {graph.version} does not have. Those
      overrides apply to nothing — they are kept rather than dropped, so nothing is lost by
      installing an older version again.
    </p>
  );
}

/* ------------------------------------------------------------ inspector -- */

function Inspector({
  projectId,
  name,
  node,
  source,
  originPath,
  ejected,
}: {
  projectId: string;
  name: string;
  node: GraphNode | null;
  source: Graph["source"];
  originPath: string | null;
  ejected: boolean;
}) {
  const overlay = useSetNodeOverlay();
  const [guarding, setGuarding] = useState(false);

  if (node === null) {
    return (
      <aside
        aria-label="Node inspector"
        className="w-full shrink-0 rounded-lg border border-border bg-surface p-4 lg:w-80"
      >
        <p className="text-xs text-text-faint">
          Pick a node to see what it is, and what this project has changed about it.
        </p>
      </aside>
    );
  }

  const tone = nodeTone(node.type, node.role);

  return (
    <aside
      aria-label="Node inspector"
      className="flex w-full shrink-0 flex-col gap-3 rounded-lg border border-border bg-surface p-4 lg:w-80"
    >
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="font-display text-base text-text">{node.label}</span>
        <span className="text-xs" style={{ color: `var(--tone-${tone}-fg)` }}>
          {node.role === "gate" ? "gate" : node.type}
        </span>
        {node.overridden ? (
          <span className="rounded-pill border border-border px-1.5 text-xs text-text-muted">
            project
          </span>
        ) : null}
      </div>
      <p className="text-xs text-text-muted">{nodeMeaning(node.type, node.role)}.</p>

      {/*
        Every field, effective value first, with what the origin said beside it when this project
        changed it. §6.2's requirement is that the origin's value is SHOWN and not merely replaced —
        an override you cannot see the other half of is a fork nobody can undo.
      */}
      <dl className="flex flex-col gap-1.5">
        {node.fields.map((field) => (
          <div key={field.name} className="flex flex-col">
            <dt className="text-xs uppercase tracking-wide text-text-faint">{field.name}</dt>
            <dd className="font-mono text-xs break-words text-text">{field.value}</dd>
            {field.origin === undefined ? null : (
              <dd className="font-mono text-xs text-text-faint">
                {field.origin === "" ? "the bundle sets nothing here" : `the bundle says ${field.origin}`}
              </dd>
            )}
          </div>
        ))}
      </dl>

      {/*
        The overlay: this project's to change, no copy taken. Deliberately NOT behind the eject
        guard — that guard is for changing what the node says, and putting it in front of a model
        change is how every project ends up ejected over one word.
      */}
      <div className="flex flex-col gap-2 border-t border-border pt-3">
        <label className="flex items-center gap-2 text-xs text-text-muted">
          <input
            type="checkbox"
            checked={node.disabled}
            onChange={(event) =>
              overlay.mutate({
                projectId,
                name,
                node: node.id,
                disabled: event.target.checked ? true : null,
              })
            }
          />
          skip this node in this project
        </label>
        <ModelField
          key={`${node.id}-model`}
          label="model"
          value={node.fields.find((field) => field.name === "model")?.value ?? ""}
          onSave={(model) => overlay.mutate({ projectId, name, node: node.id, model })}
        />
        <ModelField
          key={`${node.id}-tool`}
          label="tool"
          value={node.fields.find((field) => field.name === "tool")?.value ?? ""}
          onSave={(tool) => overlay.mutate({ projectId, name, node: node.id, tool })}
        />
        {node.overridden ? (
          <span className="self-start">
            <Button
              variant="quiet"
              onClick={() =>
                overlay.mutate({
                  projectId,
                  name,
                  node: node.id,
                  disabled: null,
                  model: null,
                  tool: null,
                  command: null,
                })
              }
            >
              follow the bundle again
            </Button>
          </span>
        ) : null}
        {/*
          A refusal is the middle weight of the escalation ladder, and it was being drawn at the
          top: a full Wrong Red box, which is what an error gets. `isApiRefusal` has already
          established that the núcleo declined this on purpose and said why — nothing is broken —
          so it takes `RefusalNote`, which is the 3px Stated Blue rule and the code in mono. The
          code is the part the hand-rolled box dropped, and it is the only thing here that can be
          searched for.

          The `kill_switch` sentence stays this route's own, because only this route knows that the
          thing being refused writes into the project's folder. Every other code now falls through
          to the shared floor instead of to a raw `detail`.
        */}
        {overlay.isError && isApiRefusal(overlay.error) ? (
          <RefusalNote
            refusal={overlay.error}
            sentences={{
              kill_switch: "the emergency stop is engaged, and this writes into the project's folder.",
            }}
          />
        ) : null}
      </div>

      {/* Changing what the node SAYS. A different question, and the one §6.3 guards. */}
      <div className="border-t border-border pt-3">
        {node.fields.find((field) => field.name === "body") === undefined ? (
          <p className="text-xs text-text-faint">
            This node has no instructions file — there is nothing to open.
          </p>
        ) : ejected || source === "project" ? (
          <button
            type="button"
            disabled={originPath === null}
            onClick={() => originPath !== null && void openInVscode(originPath, null)}
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
          >
            edit the instructions
          </button>
        ) : guarding ? (
          <EditGuard
            originPath={originPath}
            onCancel={() => setGuarding(false)}
            onOpened={() => setGuarding(false)}
          />
        ) : (
          <button
            type="button"
            onClick={() => setGuarding(true)}
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text hover:border-border-strong"
          >
            edit the instructions
          </button>
        )}
      </div>
    </aside>
  );
}

/**
 * One overlay field, saved on blur.
 *
 * `defaultValue` and an uncontrolled input, keyed by node above: typing a model name is not
 * something to send a request per keystroke for, and a controlled field that saved on change would
 * write `o`, `op`, `opu` into somebody's repository.
 */
function ModelField({
  label,
  value,
  onSave,
}: {
  label: string;
  value: string;
  onSave: (next: string | null) => void;
}) {
  return (
    <label className="flex items-center gap-2 text-xs text-text-muted">
      <span className="w-10 shrink-0">{label}</span>
      <input
        aria-label={`${label} in this project`}
        defaultValue={value}
        spellCheck={false}
        onBlur={(event) => {
          const next = event.target.value.trim();
          if (next === value.trim()) return;
          // Emptied means *stop overriding*, which the núcleo reads the same way. There is no
          // model whose name is nothing, so the two cannot be confused.
          onSave(next === "" ? null : next);
        }}
        className="min-w-0 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-xs text-text"
      />
    </label>
  );
}

/**
 * §6.3, inline in the inspector and never a modal.
 *
 * Three exits, and the middle one is the point: **most of the time what somebody wants is to
 * improve the workflow, not to diverge from it.** Offering both side by side is what makes ejecting
 * a deliberate choice instead of the path of least resistance.
 *
 * `eject and edit` is not a button here — it is the row's, one level up, where the sentence about
 * losing updates already lives. Two places to eject from would be two places for that sentence to
 * be worded differently.
 */
function EditGuard({
  originPath,
  onCancel,
  onOpened,
}: {
  originPath: string | null;
  onCancel: () => void;
  onOpened: () => void;
}) {
  return (
    <div
      role="group"
      aria-label="Edit this node's instructions"
      className="flex flex-col gap-2 rounded-md border border-border bg-surface-sunken p-3"
    >
      <p className="text-xs text-text-muted">
        This node belongs to a bundle this project references. Editing it in the library changes it
        for every project that uses it, which is usually what improving a workflow means. Ejecting
        gives this project its own copy and stops the updates.
      </p>
      <div className="flex flex-wrap gap-2">
        <button
          type="button"
          disabled={originPath === null}
          onClick={() => {
            if (originPath === null) return;
            void openInVscode(originPath, null);
            onOpened();
          }}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          edit in the library
        </button>
        <span className="inline-flex px-2 py-1.5">
          <Button variant="quiet" onClick={onCancel}>
            cancel
          </Button>
        </span>
      </div>
      <p className="text-xs text-text-faint">
        To diverge instead, eject the workflow above — that is where the sentence about losing
        updates lives.
      </p>
    </div>
  );
}

/* ------------------------------------------------------------ miniature -- */

/**
 * §4.5's miniature: the installed workflow as a chain, with the running node lit.
 *
 * The same data as the canvas and a fraction of the ink, because the Estado mode's question is *how
 * is this now* and the answer is a glance. Reading left to right in the same order the canvas lays
 * out, so the two cannot teach different shapes for one workflow.
 */
export function WorkflowChain({
  projectId,
  name,
  running = null,
}: {
  projectId: string;
  name: string;
  running?: string | null;
}) {
  const graph = useWorkflowGraph(projectId, name);
  if (graph.data === undefined || graph.data.nodes.length === 0) return null;

  return (
    <ol className="flex flex-wrap items-center gap-1.5" aria-label={`${name} as a chain`}>
      {inSequence(graph.data.nodes, graph.data.edges).map((node) => (
        <li
          key={node.id}
          title={`${node.label} — ${nodeMeaning(node.type, node.role)}`}
          className={`flex items-center gap-1 rounded-pill border px-2 py-0.5 text-xs ${
            node.disabled ? "border-dotted opacity-50" : ""
          }`}
          style={{ borderColor: `var(--tone-${nodeTone(node.type, node.role)}-border)` }}
        >
          {running === node.id ? (
            <span aria-label="running" className="h-1 w-1 rounded-pill bg-tone-active-fg" />
          ) : null}
          <span className="text-text-muted">{node.label}</span>
        </li>
      ))}
    </ol>
  );
}
