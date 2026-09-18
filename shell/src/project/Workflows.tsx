// §spec motor-de-workflows
import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  driftingWorkflows,
  NO_WORKFLOW_MEANS,
  sinceText,
  standingSentence,
  standingTone,
  useEjectWorkflow,
  useForgetWorkflow,
  useInstallWorkflow,
  useProjectWorkflows,
  useUpdateWorkflow,
  useWorkflowDiff,
  useWorkflowLibrary,
  type Bundle,
  type Installed,
} from "../data/workflows";
import { openInVscode } from "../lib/vscode";
import { Button, Quiet, RefusalNote, Row, Rows, Section, Teach, Well } from "../ui";
import { WorkflowChain, WorkflowGraph } from "./WorkflowGraph";

/**
 * The library, the pin, and how far the two have drifted apart.
 *
 * §6.1's model in one panel: a bundle lives once in `~/.nucleos/workflows/`, a project keeps a pin
 * naming one, and ejecting takes a copy that stops receiving updates. **The canvas is not here** —
 * it arrives on top of this, and until it does the useful question this already answers is *which
 * workflow is installed in this project and is it out of date?*
 *
 * **Drift is stated, never implied.** The daemon serves both hashes when they disagree, so the page
 * says what was pinned and what is there now rather than asserting a difference nobody can check.
 * §6.1 names the freeze-in-silence as the weakness of this whole model; a line saying how long a
 * copy has been on its own is the smallest honest answer to it.
 */

export interface WorkflowsProps {
  projectId: string;
}

export function Workflows({ projectId }: WorkflowsProps) {
  const installed = useProjectWorkflows(projectId);
  const [openDiff, setOpenDiff] = useState<string | null>(null);

  if (installed.isError) {
    return <Unreadable error={installed.error} />;
  }
  if (installed.data === undefined) {
    return <p className="text-sm text-text-faint">Reading this project's workflows…</p>;
  }

  return (
    <div className="flex flex-col gap-6">
      {installed.data.length === 0 ? (
        /*
          `Teach` here and a one-line `Quiet` in the State mode, over the same claim written once.
          The two are not the same job: this is a page somebody opened meaning to install
          something, with the library right under it, and that one is a section of a page about
          how the project is now. What must not differ is what the emptiness MEANS, which is why
          the sentence comes off the data layer rather than out of this file.
        */
        <Teach title="No workflow is installed here">
          {NO_WORKFLOW_MEANS} The app does not pretend otherwise by drawing an empty graph; a
          bundle from the library below is what fills this.
        </Teach>
      ) : (
        <ul className="flex flex-col gap-3">
          {installed.data.map((row) => (
            <InstalledRow
              key={row.name}
              projectId={projectId}
              row={row}
              diffOpen={openDiff === row.name}
              onToggleDiff={() => setOpenDiff(openDiff === row.name ? null : row.name)}
            />
          ))}
        </ul>
      )}

      <Library projectId={projectId} installed={installed.data} />
    </div>
  );
}

/**
 * Why the list could not be read, in the daemon's words.
 *
 * Two of these are ordinary and one is not, and they must not read alike. A machine with no library
 * has nowhere for bundles to live; a pins file that will not parse is a file somebody has to fix,
 * and the parser's own words are what say which line — the same reason `POST /write` carries a
 * `detail`.
 */
function Unreadable({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <p className="text-sm text-text-muted">The núcleo did not answer about workflows.</p>;
  }
  if (error.code === "no_library") {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        This machine has no folder for a workflow library, so there is nothing to install from.
      </p>
    );
  }
  if (error.code === "unreadable_pins") {
    return (
      <div className="max-w-prose rounded-lg border border-tone-paused-border bg-tone-paused-bg p-4">
        <p className="text-sm text-text">
          <span className="font-mono">.ai/workflows.yaml</span> does not parse, so the núcleo cannot
          say what this project uses.
        </p>
        <p className="mt-1 text-xs text-text-muted">{error.detail}</p>
      </div>
    );
  }
  return <p className="text-sm text-text-muted">No folder is recorded for this project.</p>;
}

/* ------------------------------------------------------------- installed -- */

function InstalledRow({
  projectId,
  row,
  diffOpen,
  onToggleDiff,
}: {
  projectId: string;
  row: Installed;
  diffOpen: boolean;
  onToggleDiff: () => void;
}) {
  const eject = useEjectWorkflow();
  const update = useUpdateWorkflow();
  const forget = useForgetWorkflow();
  const library = useWorkflowLibrary();
  const [guarding, setGuarding] = useState(false);

  // Where the origin actually IS on this machine, which is not what `origin` on the pin says: that
  // is a coordinate for a machine that does not have the bundle, and this is a path for the one
  // that does. §6.3's second exit needs the second, and there is nothing to open when the library
  // has nothing at that coordinate — which is precisely the `missing` case.
  const originPath =
    library.data?.find((bundle) => bundle.name === row.name && bundle.version === row.version)
      ?.path ?? null;

  const tone = standingTone(row.standing);
  const refused = [eject, update, forget].find(
    (mutation) => mutation.isError && isApiRefusal(mutation.error),
  );

  return (
    <li
      className="flex flex-col gap-3 rounded-lg border bg-surface p-4"
      style={{ borderColor: `var(--tone-${tone}-border)` }}
    >
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="font-display text-lg text-text">{row.name}</span>
        <span className="font-mono text-xs text-text-faint">{row.version}</span>
        <span className="text-xs" style={{ color: `var(--tone-${tone}-fg)` }}>
          {row.standing}
        </span>
        {/*
          The overlay, counted. §6.2 says a disabled node stays in the graph rather than
          disappearing from it; until the canvas is here to draw that, the count is what stops an
          overridden workflow from looking identical to an untouched one.
        */}
        {row.overridden_nodes > 0 ? (
          <span className="text-xs text-text-muted">
            {row.overridden_nodes} {row.overridden_nodes === 1 ? "node" : "nodes"} overridden by this
            project
            {row.disabled_nodes > 0 ? `, ${row.disabled_nodes} switched off` : ""}
          </span>
        ) : null}
      </div>

      <p className="text-sm text-text-muted">{standingSentence(row, Date.now())}</p>
      {row.description !== null ? (
        <p className="text-xs text-text-faint">{row.description}</p>
      ) : null}

      {/*
        Both hashes, side by side, and only when they disagree. Serving them is what makes "this
        changed" a claim somebody can check instead of one they have to take on trust.
      */}
      {row.standing === "drifted" && row.origin_hash !== null ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 font-mono text-xs text-text-faint">
          <dt>pinned</dt>
          <dd>{shortHash(row.hash)}</dd>
          <dt>in the library</dt>
          <dd>{shortHash(row.origin_hash)}</dd>
        </dl>
      ) : null}

      {row.standing === "missing" ? (
        <p className="max-w-prose text-xs text-text-muted">
          The pin travelled with this repository and the bundle did not. It came from{" "}
          <span className="font-mono">{row.origin}</span> — which is what the pin records so that a
          new machine can be told what it is missing rather than quietly having no workflow.
        </p>
      ) : null}

      {row.owns.length > 0 ? (
        <p className="text-xs text-text-faint">
          Authors {row.owns.map((path) => path).join(", ")} in this project.
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        {row.update_available !== null ? (
          <button
            type="button"
            disabled={update.isPending}
            onClick={() =>
              update.mutate({ projectId, name: row.name, version: row.update_available ?? undefined })
            }
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
          >
            take {row.update_available}
          </button>
        ) : null}

        {row.standing === "drifted" ? (
          <button
            type="button"
            disabled={update.isPending}
            onClick={() => update.mutate({ projectId, name: row.name, version: row.version })}
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
          >
            follow the library again
          </button>
        ) : null}

        {row.standing === "ejected" ? (
          <>
            <button
              type="button"
              onClick={onToggleDiff}
              className="rounded-md border border-border px-3 py-1.5 text-xs text-text hover:border-border-strong"
            >
              {diffOpen ? "hide the diff" : "see the diff"}
            </button>
            {/*
              Update after the diff and never before it: for an ejected workflow this REPLACES the
              copy, which is what update means and why the thing that shows what would be lost sits
              to its left.
            */}
            <button
              type="button"
              disabled={update.isPending}
              onClick={() => update.mutate({ projectId, name: row.name })}
              className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
            >
              replace with the library's
            </button>
          </>
        ) : (
          <EjectGuard
            open={guarding}
            pending={eject.isPending}
            onOpen={() => setGuarding(true)}
            onCancel={() => setGuarding(false)}
            onEject={() => {
              setGuarding(false);
              eject.mutate({ projectId, name: row.name });
            }}
            onEditOrigin={() => setGuarding(false)}
            originPath={originPath}
          />
        )}

        <span className="ml-auto">
          <Button variant="quiet" onClick={() => forget.mutate({ projectId, name: row.name })}>
            stop using it
          </Button>
        </span>
      </div>

      {/*
        The middle weight, which is what a refusal is. This was a full Wrong Red box — the weight
        reserved for something being wrong — over a núcleo that declined on purpose and named the
        reason. `REFUSALS` survives unchanged and is handed in as this route's own sentences, which
        is the prop that exists for exactly it: same codes, sharper words, because only the route
        knows which ceiling it hit. What the primitive adds is the code itself, in mono, which none
        of the eight sentences below can be searched by.
      */}
      {refused !== undefined && isApiRefusal(refused.error) ? (
        <RefusalNote refusal={refused.error} sentences={REFUSALS} />
      ) : null}

      {diffOpen ? <Diff projectId={projectId} name={row.name} /> : null}

      {/*
        The canvas, under the row whose workflow it draws. Not a separate page and not a tab: the
        picture and the sentence about where the bundle stands are the same subject, and a surface
        that separated them would make somebody hold the standing in their head while looking at
        the graph.

        Only for a workflow this machine can actually produce a graph for. A `missing` pin has
        nothing to draw and the row above already says what it is missing.
      */}
      {row.standing === "missing" ? null : (
        <WorkflowGraph
          projectId={projectId}
          name={row.name}
          originPath={originPath}
          ejected={row.standing === "ejected"}
        />
      )}
    </li>
  );
}

const REFUSALS: Record<string, string> = {
  kill_switch: "the emergency stop is engaged, and this writes into the project's folder.",
  no_project_root: "the núcleo has no folder recorded for this project.",
  no_library: "this machine has no workflow library.",
  no_such_bundle: "the library has no bundle at that version.",
  not_installed: "this project does not use that workflow.",
  already_ejected: "this project already has its own copy — the diff is the thing to look at.",
  bad_name: "that is not a usable workflow name.",
  internal: "the núcleo hit an error of its own.",
};

/**
 * §6.3's guard, inline and never a modal.
 *
 * Three exits, and the middle one is the point: **most of the time what somebody wants is to
 * improve the workflow, not to diverge from it.** Offering both side by side is what makes ejecting
 * a deliberate choice instead of the path of least resistance — and the sentence under it says what
 * is actually given up, which is future updates rather than anything visible today.
 *
 * Inline because §3.2 of the 2026-08-17 frontend spec is older and stronger than the convenience of
 * a dialog: a modal takes the page away to ask a question about something on it.
 */
function EjectGuard({
  open,
  pending,
  originPath,
  onOpen,
  onCancel,
  onEject,
  onEditOrigin,
}: {
  open: boolean;
  pending: boolean;
  /** Where the bundle is on this machine, or `null` when this machine does not have it. */
  originPath: string | null;
  onOpen: () => void;
  onCancel: () => void;
  onEject: () => void;
  onEditOrigin: () => void;
}) {
  if (!open) {
    return (
      <button
        type="button"
        disabled={pending}
        onClick={onOpen}
        className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
      >
        eject
      </button>
    );
  }

  return (
    <div
      role="group"
      aria-label="Eject or edit in the library"
      className="flex w-full flex-col gap-2 rounded-md border border-border bg-surface-sunken p-3"
    >
      <p className="max-w-prose text-xs text-text-muted">
        Ejecting gives this project its own copy, and that copy stops receiving anything the library
        gets from then on. If the change is an improvement, the library is where it helps every
        project that uses this bundle.
      </p>
      <div className="flex flex-wrap gap-2">
        <button
          type="button"
          onClick={onEject}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text hover:border-border-strong"
        >
          eject and edit
        </button>
        <button
          type="button"
          disabled={originPath === null}
          onClick={() => {
            if (originPath === null) return;
            // The library path, not the project's. `openInVscode` is the same door layer 2 uses for
            // everything this app does not author — and a bundle in the library is the clearest
            // case of that there is.
            void openInVscode(originPath, null);
            onEditOrigin();
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
    </div>
  );
}

/** The drift, file by file. */
function Diff({ projectId, name }: { projectId: string; name: string }) {
  const diff = useWorkflowDiff(projectId, name);

  if (diff.isError) {
    return <p className="text-xs text-text-muted">The núcleo could not compare the two copies.</p>;
  }
  if (diff.data === undefined) {
    return <p className="text-xs text-text-faint">Comparing…</p>;
  }
  if (diff.data.changes.length === 0) {
    return (
      <p className="text-xs text-text-muted">
        Identical to {diff.data.origin_version} in the library, file for file — but still frozen: an
        ejected copy receives nothing new whether or not it has been edited.
      </p>
    );
  }

  /*
    A well, and it was a well with a border — which is the one mistake `Well` exists to stop, because
    an outline makes a box read as sitting ON the surface rather than as being cut into it, and a
    third bordered box inside a bordered row inside a page is the boxes-in-boxes the system refuses.

    `reads` because the two halves have two authors. The sentence framing the comparison is prose
    somebody wrote; the file list under it is the núcleo's answer and carries its own mono, so the
    face keeps saying which is which instead of setting the whole box in the machine's.
  */
  return (
    <Well as="div" reads>
      <p className="text-xs text-text-muted">
        Against {diff.data.origin_version} in the library. File by file — the line-by-line answer is
        the editor, one click away.
      </p>
      <ul className="mt-2 flex flex-col gap-0.5">
        {diff.data.changes.map((change) => (
          <li key={change.path} className="flex items-baseline gap-2 font-mono text-xs">
            <span className="w-14 shrink-0" style={{ color: `var(--tone-${CHANGE_TONE[change.change]}-fg)` }}>
              {change.change}
            </span>
            <span className="text-text-muted">{change.path}</span>
          </li>
        ))}
      </ul>
      {diff.data.unchanged > 0 ? (
        <p className="mt-2 text-xs text-text-faint">
          {diff.data.unchanged} other {diff.data.unchanged === 1 ? "file" : "files"} identical.
        </p>
      ) : null}
    </Well>
  );
}

const CHANGE_TONE: Record<string, string> = {
  added: "active",
  removed: "danger",
  changed: "paused",
};

/* --------------------------------------------------------------- library -- */

/** What is on this machine, and what installing one would mean. */
function Library({ projectId, installed }: { projectId: string; installed: Installed[] }) {
  const library = useWorkflowLibrary();
  const install = useInstallWorkflow();

  if (library.isError) {
    // Already said above when the project's own listing failed for the same reason; saying it twice
    // would be the page arguing with itself.
    return null;
  }
  if (library.data === undefined) {
    return <p className="text-xs text-text-faint">Reading the library…</p>;
  }

  const pinned = new Map(installed.map((row) => [row.name, row.version]));

  /*
    `Section` at rank 3, because this sits under the project mode's own heading rather than beside
    it. The hand-rolled pair was already the shared recipe to the declaration — display face, 11px,
    500, `--tracking-wider`, uppercase, faint — which is drift rather than a gap, and the drift is
    what a seventh copy gets to disagree with by accident. The region now answers to its own
    heading instead of to "The library", which is a name nothing on screen ever said.
  */
  return (
    <Section label="On this machine" level={3}>
      {library.data.length === 0 ? (
        <p className="max-w-prose text-sm text-text-muted">
          The library is empty. A bundle is a folder in{" "}
          <span className="font-mono">~/.nucleos/workflows/&lt;name&gt;/&lt;version&gt;/</span> with a{" "}
          <span className="font-mono">bundle.yaml</span> in it; nothing here creates one, because
          authoring is the second half of this design and not this one.
        </p>
      ) : (
        /*
          A hairline-ruled column, and it was a stack of bordered boxes. Every bundle on the machine
          is one row of one list somebody scans to find the one they want; fifty of them as separate
          cards is a pile, and the eye stops reading a pile as a list. The rules are a 1px gap over a
          `--border` ground, which is why a `Row` paints its own fill.
        */
        <Rows label="Workflow bundles on this machine">
          {library.data.map((bundle) => (
            <LibraryRow
              key={`${bundle.name}@${bundle.version}`}
              bundle={bundle}
              pinnedVersion={pinned.get(bundle.name) ?? null}
              pending={install.isPending}
              onInstall={() =>
                install.mutate({
                  projectId,
                  name: bundle.name,
                  version: bundle.version,
                })
              }
            />
          ))}
        </Rows>
      )}
      {install.isError && isApiRefusal(install.error) ? (
        <RefusalNote refusal={install.error} sentences={REFUSALS} />
      ) : null}
    </Section>
  );
}

function LibraryRow({
  bundle,
  pinnedVersion,
  pending,
  onInstall,
}: {
  bundle: Bundle;
  pinnedVersion: string | null;
  pending: boolean;
  onInstall: () => void;
}) {
  const isPinned = pinnedVersion === bundle.version;

  /*
    `layout="line"` because the parts sit on one baseline: name, version, description, and the
    gesture pushed to the far end. That is an axis the primitive names rather than a `className` it
    would have had to accept — a row must not be handed the one property it owns, its background,
    which is what keeps the container's hairline ground from showing through it.
  */
  return (
    <Row layout="line">
      <span className="text-sm text-text">{bundle.name}</span>
      <span className="font-mono text-xs text-text-faint">{bundle.version}</span>
      {bundle.description !== null ? (
        <span className="text-xs text-text-muted">{bundle.description}</span>
      ) : null}
      {isPinned ? (
        <span className="ml-auto text-xs text-text-muted">used here</span>
      ) : (
        <button
          type="button"
          disabled={pending}
          onClick={onInstall}
          className="ml-auto rounded-md border border-border px-2.5 py-1 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          {/*
            One workflow per name, so installing a different version of one already in use is a
            change rather than an addition — and the button says which, because "install" over
            something that is working is the click somebody regrets.
          */}
          {pinnedVersion === null ? "use here" : `use ${bundle.version} instead`}
        </button>
      )}
    </Row>
  );
}

/**
 * The Estado mode's panel: what is installed, and whether anything about it needs looking at.
 *
 * Not the whole surface above. §4.5 asks for the installed workflow as a chain with the running
 * node lit, and the chain needs the graph — so what stands here until then is the half that is
 * already answerable, plus a line saying what is missing and why.
 */
export function WorkflowSummary({ projectId }: { projectId: string }) {
  const installed = useProjectWorkflows(projectId);

  if (installed.isError || installed.data === undefined) {
    return (
      <p className="text-sm text-text-faint">
        {installed.isError
          ? "The núcleo did not say which workflows this project uses."
          : "Reading this project's workflows…"}
      </p>
    );
  }

  if (installed.data.length === 0) {
    /*
      A dashed box around two sentences was the loudest thing on this page about the one section
      with nothing in it. The sentence that makes the emptiness a state rather than a gap is kept
      and moved behind the question; what is left is the fact.
    */
    return <Quiet says="none installed">{NO_WORKFLOW_MEANS}</Quiet>;
  }

  return (
    <div className="flex flex-col gap-2">
      {installed.data.map((row) => (
        <div
          key={row.name}
          className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-lg border bg-surface px-4 py-3"
          style={{ borderColor: `var(--tone-${standingTone(row.standing)}-border)` }}
        >
          <span className="text-sm text-text">{row.name}</span>
          <span className="font-mono text-xs text-text-faint">{row.version}</span>
          <span className="text-xs text-text-muted">{standingSentence(row, Date.now())}</span>
          {row.ejected_at !== null && row.standing === "ejected" ? (
            <span className="ml-auto text-xs text-text-faint">
              frozen {sinceText(row.ejected_at, Date.now())}
            </span>
          ) : null}
          {/*
            §4.5's miniature. The same reading as the canvas in the Workflows mode, at a fraction of
            the ink — the question here is *how is this now*, and the answer is a glance.

            Nothing lights yet: which node a run is on is execution semantics, which §14 keeps for
            the second spec. The chain is drawn now because the shape of the workflow is worth
            knowing on its own, and because a lit node is then one prop rather than a redraw.
          */}
          {row.standing === "missing" ? null : (
            <div className="basis-full">
              <WorkflowChain projectId={projectId} name={row.name} />
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

/** Enough of a hash to compare two by eye, which is all this is ever used for. */
function shortHash(hash: string): string {
  return hash.replace(/^sha256:/, "").slice(0, 12);
}

/** Re-exported so `ModeState` can lead with drift without importing the data layer twice. */
export { driftingWorkflows };
