// §spec motor-de-workflows
import { useEffect, useId, useRef, useState } from "react";
import { ShapeIcon, WorkflowCanvas, type SelectedBy } from "../canvas/WorkflowCanvas";
import { inSequence, nodeMeaning, nodeShape, SHAPES } from "../canvas/workflow-model";
import { isApiRefusal } from "../data/client";
import {
  useSetNodeOverlay,
  useWorkflowGraph,
  type GraphEdge,
  type GraphNode,
  type WorkflowGraph as Graph,
} from "../data/workflow-graph";
import { openInVscode } from "../lib/vscode";
import { Button, ErrorNote, Quiet, RefusalNote, Well } from "../ui";

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

/**
 * An inline panel that gives focus back to what opened it.
 *
 * Both guards on this surface replace the button that opened them, so the moment one opens, the
 * element that had focus is gone and focus falls to the document — and closing it again left
 * somebody on the keyboard at the top of the page. PRODUCT.md names the project switcher's
 * behaviour as the pattern: Escape closes, and focus returns to the trigger. The trigger is found
 * by id rather than by ref because `Button` does not forward one.
 */
export function useInlinePanel(triggerId: string) {
  const [open, setOpen] = useState(false);
  const giveBack = useRef(false);

  useEffect(() => {
    if (open || !giveBack.current) return;
    giveBack.current = false;
    document.getElementById(triggerId)?.focus();
  }, [open, triggerId]);

  return {
    open,
    show: () => setOpen(true),
    /** `returnFocus: false` when what closed it also removed the trigger — an eject, say. */
    close: (returnFocus = true) => {
      giveBack.current = returnFocus;
      setOpen(false);
    },
  };
}

/** Focus the panel itself on arrival, so what it says is the next thing read. */
export function useFocusOnMount<T extends HTMLElement>(when = true) {
  const self = useRef<T>(null);
  useEffect(() => {
    if (when) self.current?.focus();
    // Once, on arrival: re-running this on a re-render would drag focus back from wherever
    // somebody had moved it inside the panel.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return self;
}

export function WorkflowGraph({ projectId, name, originPath, ejected }: WorkflowGraphProps) {
  const graph = useWorkflowGraph(projectId, name);
  const [selected, setSelected] = useState<string | null>(null);
  // Whether the node now open was opened from the keyboard, which is when focus should follow it
  // into the inspector. A click leaves focus where the pointer is.
  const [byKey, setByKey] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  if (graph.isError) return <GraphMissing error={graph.error} />;
  if (graph.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the graph…</p>;
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

  function select(id: string | null, via: SelectedBy = "pointer") {
    setSelected(id);
    setByKey(via === "keyboard");
  }

  /** Close the inspector and put focus back on the node it was open on — where it came from. */
  function closeInspector() {
    const was = selected;
    select(null);
    if (was === null) return;
    root.current
      ?.querySelector<HTMLElement>(`.react-flow__node[data-id="${was.replace(/["\\]/g, "\\$&")}"]`)
      ?.focus();
  }

  /*
    One level of box, and it is the canvas. The inspector used to be a second bordered card beside
    it, inside a bordered card for the row, with a third bordered box for its guard — the three
    levels DESIGN.md forbids. It is now a column divided from the canvas by a hairline.

    `@container` and a 60rem query, not `lg:`. `lg` is Tailwind's 64rem, a breakpoint nobody in this
    system chose; 60rem is the app's one, and measured against this component's own width rather
    than the window's, because the sidebar decides how much of the window this ever gets.
  */
  return (
    <div ref={root} className="@container flex flex-col gap-3">
      <Orphaned graph={graph.data} />
      <div className="flex flex-col gap-4 @min-[60rem]:flex-row">
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <WorkflowCanvas
            nodes={graph.data.nodes}
            edges={graph.data.edges}
            selected={selected}
            onSelect={select}
          />
          <WorkflowKey nodes={graph.data.nodes} edges={graph.data.edges} />
        </div>
        <Inspector
          // Keyed by node: the save line, the guard and the fields belong to the node they were
          // opened on, and must not carry over to the next one.
          key={node?.id ?? ""}
          projectId={projectId}
          name={name}
          node={node}
          source={graph.data.source}
          originPath={originPath}
          ejected={ejected}
          takeFocus={byKey}
          onClose={closeInspector}
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
    return (
      <ErrorNote>
        The núcleo did not answer about this graph
        {error instanceof Error && error.message !== "" ? `: ${error.message}` : "."}
      </ErrorNote>
    );
  }
  if (error.code === "no_graph") {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        This bundle has no <span className="font-mono">graph.yaml</span> in it. Skills and scripts
        with no sequence yet is a halfway state, not a broken bundle.
      </p>
    );
  }
  if (error.code === "invalid_graph") {
    return (
      <div className="max-w-prose rounded-md border border-tone-paused-border bg-tone-paused-bg p-3">
        <p className="text-sm text-text">
          <span className="font-mono">graph.yaml</span> does not parse, so nothing here can be drawn
          honestly.
        </p>
        <p className="mt-1 font-mono text-xs text-text-muted">{error.detail}</p>
      </div>
    );
  }
  return <p className="text-sm text-text-muted">{error.detail}</p>;
}

/**
 * Overrides applying to nothing.
 *
 * §12 asks for `switched off here` ≠ `not in the bundle`. The first is dotted on the canvas; the
 * second cannot be drawn at all, so it is said here — and this is the only moment somebody learns
 * that a bundle they updated dropped the node they had configured.
 *
 * A well and not an ember box: nothing has stopped, so Held Ember was a claim this is not making.
 * The ids are a list in mono rather than a comma-joined run of body text, because they are the
 * núcleo's names and there can be a dozen of them.
 */
function Orphaned({ graph }: { graph: Graph }) {
  if (graph.orphaned.length === 0) return null;
  const count = graph.orphaned.length;
  return (
    <Well as="div" reads>
      <p>
        This project overrides {count === 1 ? "a node" : `${count} nodes`} that{" "}
        <span className="font-mono">{graph.version}</span> does not have. Those overrides apply to
        nothing; they are kept rather than dropped, so installing an older version again loses none
        of them.
      </p>
      <ul
        aria-label="Overrides that apply to nothing"
        className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-xs text-text-muted"
      >
        {graph.orphaned.map((id) => (
          <li key={id}>{id}</li>
        ))}
      </ul>
    </Well>
  );
}

/**
 * The key, under the canvas, listing only what this graph actually draws.
 *
 * DESIGN.md: a glyph vocabulary carries a key where it is read. The node already says its kind in
 * a word, so the key's real work is the marks that have no word on them — the dashed fail path,
 * the dotted edge of a node switched off here, the seal. A mark this graph does not use is left
 * out: a key longer than the picture is its own kind of noise.
 */
function WorkflowKey({ nodes, edges }: { nodes: GraphNode[]; edges: GraphEdge[] }) {
  const drawn = new Set(nodes.map((node) => nodeShape(node.type, node.role)));
  const marks = [
    edges.some((edge) => edge.verdict === "fail") ? "dashed line: the fail path" : null,
    nodes.some((node) => node.disabled) ? "dotted edge: off in this project" : null,
    nodes.some((node) => node.overridden) ? "“project”: changed by this project" : null,
  ].filter((mark): mark is string => mark !== null);

  return (
    <ul aria-label="Key" className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-text-muted">
      {SHAPES.filter((shape) => drawn.has(shape)).map((shape) => (
        <li key={shape} className="flex items-center gap-1">
          <ShapeIcon shape={shape} />
          <span className="text-text">{shape}</span>
          <span>— {nodeMeaning(shape === "gate" ? "command" : shape, shape === "gate" ? "gate" : "plain")}</span>
        </li>
      ))}
      {marks.map((mark) => (
        <li key={mark}>{mark}</li>
      ))}
    </ul>
  );
}

/* ------------------------------------------------------------ inspector -- */

/** The column the inspector is drawn in, open or empty: a hairline, never a box. */
const INSPECTOR =
  "flex w-full shrink-0 flex-col gap-3 border-t border-border pt-3 @min-[60rem]:w-80 @min-[60rem]:border-t-0 @min-[60rem]:border-l @min-[60rem]:pt-0 @min-[60rem]:pl-4";

function Inspector({
  projectId,
  name,
  node,
  source,
  originPath,
  ejected,
  takeFocus,
  onClose,
}: {
  projectId: string;
  name: string;
  node: GraphNode | null;
  source: Graph["source"];
  originPath: string | null;
  ejected: boolean;
  /** Opened from the keyboard: move focus to the node's name so the inspector is what is read next. */
  takeFocus: boolean;
  onClose: () => void;
}) {
  const overlay = useSetNodeOverlay();
  const editId = useId();
  const guard = useInlinePanel(editId);
  const heading = useFocusOnMount<HTMLHeadingElement>(takeFocus && node !== null);

  if (node === null) {
    return (
      <aside aria-label="Node inspector" className={INSPECTOR}>
        <p className="text-sm text-text-muted">
          Pick a node to see what it is and what this project has changed about it — click it, or
          Tab to it and press Enter.
        </p>
      </aside>
    );
  }

  const shape = nodeShape(node.type, node.role);

  return (
    <aside
      aria-label="Node inspector"
      className={INSPECTOR}
      onKeyDown={(event) => {
        // Escape closes the inspector and hands focus back to the node. A guard inside handles its
        // own Escape first and stops it, so one press closes one thing.
        if (event.key !== "Escape") return;
        event.preventDefault();
        onClose();
      }}
    >
      <div className="flex flex-wrap items-baseline gap-2">
        <h4 ref={heading} tabIndex={-1} className="font-display text-base text-text">
          {node.label}
        </h4>
        <span className="flex items-center gap-1 self-center text-xs text-text-muted">
          <ShapeIcon shape={shape} />
          {shape}
        </span>
        {node.overridden ? (
          <span className="rounded-sm border border-border px-1 text-xs text-text-muted">
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
            // The neutral ladder's top rung, not the browser's blue: an unstyled checkbox was the one
            // control on this surface wearing a colour the system never chose.
            className="accent-text"
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
          label="model"
          value={node.fields.find((field) => field.name === "model")?.value ?? ""}
          onSave={(model) => overlay.mutate({ projectId, name, node: node.id, model })}
        />
        <ModelField
          label="tool"
          value={node.fields.find((field) => field.name === "tool")?.value ?? ""}
          onSave={(tool) => overlay.mutate({ projectId, name, node: node.id, tool })}
        />
        {/*
          What happened to the last change, said where it was made. A field that saves on blur and
          then says nothing leaves somebody reopening the node to find out whether it took — and a
          screen reader user with no way to find out at all. Always mounted, so the region exists
          before the words arrive and the words are announced.
        */}
        <p role="status" className="text-xs text-text-muted">
          {overlay.isPending ? (
            "Saving…"
          ) : overlay.isSuccess ? (
            <>
              Saved to this project&rsquo;s <span className="font-mono">workflows.yaml</span>.
            </>
          ) : null}
        </p>
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
          A refusal is the middle weight of the escalation ladder: `isApiRefusal` has already
          established that the núcleo declined this on purpose and said why, so it takes
          `RefusalNote`, with the code in mono. The `kill_switch` sentence stays this route's own,
          because only this route knows that the thing refused writes into the project's folder.

          Anything else is a failure, and it was being dropped: the núcleo going away between a
          blur and its answer left the field showing a value that was never saved, with nothing on
          screen to say so.
        */}
        {overlay.isError ? (
          isApiRefusal(overlay.error) ? (
            <RefusalNote
              refusal={overlay.error}
              sentences={{
                kill_switch: "the emergency stop is engaged, and this writes into the project's folder.",
              }}
            />
          ) : (
            <ErrorNote>
              The núcleo did not answer, so this change may not have been saved
              {overlay.error instanceof Error && overlay.error.message !== ""
                ? `: ${overlay.error.message}`
                : "."}
            </ErrorNote>
          )
        ) : null}
      </div>

      {/* Changing what the node SAYS. A different question, and the one §6.3 guards. */}
      <div className="border-t border-border pt-3">
        {node.fields.find((field) => field.name === "body") === undefined ? (
          <p className="text-xs text-text-muted">
            This node has no instructions file — there is nothing to open.
          </p>
        ) : ejected || source === "project" ? (
          <Button
            disabled={originPath === null}
            onClick={() => originPath !== null && void openInVscode(originPath, null)}
          >
            edit the instructions
          </Button>
        ) : guard.open ? (
          <EditGuard
            originPath={originPath}
            onCancel={() => guard.close()}
            onOpened={() => guard.close()}
          />
        ) : (
          <Button id={editId} onClick={guard.show}>
            edit the instructions
          </Button>
        )}
      </div>
    </aside>
  );
}

/**
 * One overlay field, saved on blur or on Enter.
 *
 * `defaultValue` and an uncontrolled input, keyed by node above: typing a model name is not
 * something to send a request per keystroke for, and a controlled field that saved on change would
 * write `o`, `op`, `opu` into somebody's repository.
 *
 * Escape puts the value back before anything is sent, and lets the key through so the inspector
 * closes behind it — the blur that follows then finds nothing changed. `sent` is what stops Enter
 * and the blur after it from saving the same value twice.
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
  const sent = useRef(value.trim());

  function commit(raw: string) {
    const next = raw.trim();
    if (next === sent.current) return;
    sent.current = next;
    // Emptied means *stop overriding*, which the núcleo reads the same way. There is no model
    // whose name is nothing, so the two cannot be confused.
    onSave(next === "" ? null : next);
  }

  return (
    <label className="flex items-center gap-2 text-xs text-text-muted">
      <span className="w-10 shrink-0">{label}</span>
      <input
        aria-label={`${label} in this project`}
        defaultValue={value}
        spellCheck={false}
        onBlur={(event) => commit(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            commit(event.currentTarget.value);
          } else if (event.key === "Escape") {
            event.currentTarget.value = sent.current;
          }
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
 *
 * A well rather than a bordered box: this sits inside the inspector's column, and a third outline
 * there was the boxes-in-boxes the system refuses. The classes are `Well`'s own, written out,
 * because the primitive does not take the ref, role and key handler a focusable group needs.
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
  const self = useFocusOnMount<HTMLDivElement>();
  return (
    <div
      ref={self}
      role="group"
      aria-label="Edit this node's instructions"
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        // One press, one thing closed: the guard, and not the inspector around it.
        event.stopPropagation();
        onCancel();
      }}
      className="ui-well ui-well-reading flex flex-col gap-2"
    >
      <p>
        This node belongs to a bundle this project references. Editing it in the library changes it
        for every project that uses it, which is usually what improving a workflow means. Ejecting
        gives this project its own copy and stops the updates.
      </p>
      <div className="flex flex-wrap items-center gap-3">
        <Button
          disabled={originPath === null}
          onClick={() => {
            if (originPath === null) return;
            void openInVscode(originPath, null);
            onOpened();
          }}
        >
          edit in the library
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          cancel
        </Button>
      </div>
      <p className="text-xs text-text-muted">To diverge instead, eject the workflow above.</p>
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
          // Neutral, with the kind as the canvas's own icon: the tone this carried per kind was the
          // canvas's violet-and-amber, and it is gone from both for the same reason. The 3px rung,
          // because a pill is what a badge looks like. A gate keeps its heavier edge.
          className={`flex items-center gap-1 rounded-sm border px-2 py-0.5 text-xs ${
            node.role === "gate" ? "border-border-strong" : "border-border"
          } ${node.disabled ? "border-dotted opacity-50" : ""}`}
        >
          {running === node.id ? (
            <span role="img" aria-label="running" className="h-1 w-1 rounded-pill bg-tone-active-fg" />
          ) : null}
          <ShapeIcon shape={nodeShape(node.type, node.role)} />
          <span className="text-text-muted">{node.label}</span>
        </li>
      ))}
    </ol>
  );
}
