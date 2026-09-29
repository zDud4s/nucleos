// §spec motor-de-workflows
import { useEffect, useId, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { isApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import {
  driftingWorkflows,
  LIBRARY_HINT,
  libraryRootOf,
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
import {
  Button,
  ConfirmButton,
  CopyButton,
  ErrorNote,
  Quiet,
  RefusalNote,
  Row,
  Rows,
  Section,
  StaleNote,
  Teach,
  Well,
} from "../ui";
import { useFocusOnMount, useInlinePanel, WorkflowChain, WorkflowGraph } from "./WorkflowGraph";

/**
 * The library, the pin, and how far the two have drifted apart.
 *
 * §6.1's model on one page: a bundle lives once in `~/.nucleos/workflows/`, a project keeps a pin
 * naming one, and ejecting takes a copy that stops receiving updates. The canvas is drawn under the
 * workflow it pictures, and the question this page answers first is *which workflow is installed in
 * this project and is it out of date?*
 *
 * **Drift is stated, never implied.** The daemon serves both hashes when they disagree, so the page
 * says what was pinned and what is there now rather than asserting a difference nobody can check.
 * §6.1 names the freeze-in-silence as the weakness of this whole model; a line saying how long a
 * copy has been on its own is the smallest honest answer to it — and it has to keep counting while
 * the page is open, which is what `useNow` is for.
 */

export interface WorkflowsProps {
  projectId: string;
}

/**
 * The clock, re-read once a minute.
 *
 * The standing sentences are computed against "now", and a page left open over a weekend used to go
 * on saying "receiving no updates for 3 days" because `Date.now()` was only read at render. The
 * sentence is coarse — days at the finest — so a minute is far finer than it can show and costs
 * nothing.
 */
function useNow(every = 60_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const tick = window.setInterval(() => setNow(Date.now()), every);
    return () => window.clearInterval(tick);
  }, [every]);
  return now;
}

/** An error's own words after a colon, or a full stop when it has none. */
function said(error: unknown): string {
  return error instanceof Error && error.message !== "" ? `: ${error.message}` : ".";
}

export function Workflows({ projectId }: WorkflowsProps) {
  const installed = useProjectWorkflows(projectId);
  const library = useWorkflowLibrary();
  const queryClient = useQueryClient();
  const now = useNow();
  const [openDiff, setOpenDiff] = useState<string | null>(null);

  // Everything this page read, read again: the pins with their diffs and graphs under the project
  // prefix, and the library beside them.
  function reread() {
    void queryClient.invalidateQueries({ queryKey: keys.projects.workflows(projectId) });
    void queryClient.invalidateQueries({ queryKey: keys.workflows.all });
  }

  // A refusal is an answer and replaces the page; a failure after a good read keeps the good read on
  // screen and says it is old, rather than throwing away what was true a minute ago.
  if (installed.isError && (installed.data === undefined || isApiRefusal(installed.error))) {
    return <Unreadable error={installed.error} onRetry={reread} />;
  }
  if (installed.data === undefined) {
    return <p className="text-sm text-text-faint">Reading this project's workflows…</p>;
  }

  const nothing = installed.data.length === 0;

  return (
    <div className="flex flex-col gap-8">
      {installed.isError ? <StaleNote dataUpdatedAt={installed.dataUpdatedAt} /> : null}

      {nothing ? (
        <NothingInstalled library={library.data} />
      ) : (
        installed.data.map((row) => (
          <InstalledRow
            key={row.name}
            projectId={projectId}
            row={row}
            now={now}
            diffOpen={openDiff === row.name}
            onToggleDiff={() => setOpenDiff(openDiff === row.name ? null : row.name)}
          />
        ))
      )}

      {/* Folded into the empty state above when both are empty: two stacked nothings said the same
          thing twice, in two sizes, at two different indents. */}
      <Library projectId={projectId} installed={installed.data} quietWhenEmpty={nothing} />

      <ReadAt at={installed.dataUpdatedAt} onReread={reread} />
    </div>
  );
}

/**
 * Nothing installed, and the way out of that.
 *
 * `Teach` here and a one-line `Quiet` in the State mode, over the same claim written once. The two
 * are not the same job: this is a page somebody opened meaning to install something, and that one
 * is a section of a page about how the project is now. What must not differ is what the emptiness
 * MEANS, which is why the sentence comes off the data layer rather than out of this file.
 *
 * The exit is the part that was missing — `Teach` is a title, a sentence and a way out, and it had
 * lost the third. With bundles on the machine the list under this is the way, and the folder they
 * live in opens beside it for somebody who means to write one. With none, the folder cannot be
 * opened (the daemon does not serve its absolute path, and the webview is not the authority on
 * where home is), so its path is offered to copy instead.
 *
 * The wrapper strips the teach block's inline padding. It is there for a Teach that is the whole of
 * a page; here it sits over a section whose heading starts flush, and the title began 20px to the
 * right of it — the same double indent `.ui-panel-body > .ui-teach` removes inside a panel.
 */
function NothingInstalled({ library }: { library: Bundle[] | undefined }) {
  const root = libraryRootOf(library);
  const empty = library !== undefined && library.length === 0;

  const action =
    root !== null ? (
      <Button onClick={() => void openInVscode(root, null)}>Open the library folder</Button>
    ) : empty ? (
      <LibraryPath />
    ) : undefined;

  return (
    <div className="[&>.ui-teach]:px-0!">
      <Teach title="No workflow is installed here" action={action}>
        {NO_WORKFLOW_MEANS}{" "}
        {empty ? (
          <>
            Workflows come from bundles in your library: a folder under{" "}
            <span className="font-mono">~/.nucleos/workflows/&lt;name&gt;/&lt;version&gt;/</span>{" "}
            with a <span className="font-mono">bundle.yaml</span> in it. This machine has none yet.
          </>
        ) : (
          "Pick one from this machine's library below."
        )}
      </Teach>
    </div>
  );
}

/**
 * The library's path, shown, with a copy control beside it.
 *
 * The path is the visible label and the button is only its icon. `CopyButton`'s own word is a bare
 * "Copy", which on a lone button says nothing about what it copies — its accessible name did, so
 * the eye and the screen reader were told two different things (WCAG 2.5.3). And a lone button
 * started a padding's width to the right of the text column above it; with the path first, the row
 * starts flush and the button's padding falls after the text instead of before it.
 */
function LibraryPath() {
  return (
    <span className="inline-flex items-center gap-1">
      <span className="font-mono text-sm text-text">{LIBRARY_HINT}</span>
      <CopyButton value={LIBRARY_HINT} label="the library path" spoken={false} />
    </span>
  );
}

/**
 * When the page last heard from the núcleo, and how to hear again.
 *
 * Nothing here polls — see `data/workflows.ts` — so the page says when it read, the one thing that
 * makes an unpolled view honest. It is re-read after every change made here and when the window
 * regains focus; a change made somewhere else (an editor, a `git pull`) shows only then, or when
 * asked for.
 */
function ReadAt({ at, onReread }: { at: number; onReread: () => void }) {
  if (at <= 0) return null;
  const when = new Date(at);
  return (
    <p className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-xs text-text-muted">
      <span>
        Read at{" "}
        <time className="font-mono tabular-nums" dateTime={when.toISOString()}>
          {when.toTimeString().slice(0, 5)}
        </time>
        , and again after each change made here or when this window regains focus.
      </span>
      <Button variant="quiet" onClick={onReread}>
        read again
      </Button>
    </p>
  );
}

/**
 * Why the list could not be read, in the daemon's words.
 *
 * Two of these are ordinary and one is not, and they must not read alike. A machine with no library
 * has nowhere for bundles to live; a pins file that will not parse is a file somebody has to fix,
 * and the parser's own words are what say which line — the same reason `POST /write` carries a
 * `detail`. A núcleo that did not answer at all is a failure, and says what it was and how to ask
 * again: PRODUCT.md reads an error here as a bug report, and a bug report with no detail is noise.
 */
function Unreadable({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (!isApiRefusal(error)) {
    return (
      <div className="flex flex-col items-start gap-2">
        <ErrorNote>The núcleo did not answer about this project's workflows{said(error)}</ErrorNote>
        <Button onClick={onRetry}>Try again</Button>
      </div>
    );
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
          This project&rsquo;s <span className="font-mono">workflows.yaml</span> does not parse, so
          the núcleo cannot say what this project uses.
        </p>
        <p className="mt-1 font-mono text-xs text-text-muted">{error.detail}</p>
      </div>
    );
  }
  return <p className="text-sm text-text-muted">No folder is recorded for this project.</p>;
}

/* ------------------------------------------------------------- installed -- */

/**
 * One installed workflow: a heading, what it stands on, what can be done, and its picture.
 *
 * A section and not a card. This was a bordered box with a tone-coloured edge, holding a bordered
 * canvas, holding a bordered inspector, holding a bordered guard — the boxes-in-boxes DESIGN.md
 * calls the system's most common structural violation. The heading carries the grouping now, and
 * the canvas is the one box left, because it is an instrument and needs an edge to be read against.
 */
function InstalledRow({
  projectId,
  row,
  now,
  diffOpen,
  onToggleDiff,
}: {
  projectId: string;
  row: Installed;
  now: number;
  diffOpen: boolean;
  onToggleDiff: () => void;
}) {
  const eject = useEjectWorkflow();
  const update = useUpdateWorkflow();
  const forget = useForgetWorkflow();
  const library = useWorkflowLibrary();
  const ejectId = useId();
  const guard = useInlinePanel(ejectId);

  // Where the origin actually IS on this machine, which is not what `origin` on the pin says: that
  // is a coordinate for a machine that does not have the bundle, and this is a path for the one
  // that does. §6.3's second exit needs the second, and there is nothing to open when the library
  // has nothing at that coordinate — which is precisely the `missing` case.
  const originPath =
    library.data?.find((bundle) => bundle.name === row.name && bundle.version === row.version)
      ?.path ?? null;

  const tone = standingTone(row.standing);
  const failed = [eject, update, forget].find((mutation) => mutation.isError);
  const overrides = row.overridden_nodes;

  return (
    <section aria-label={`${row.name} workflow`} className="flex flex-col gap-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h3 className="font-display text-lg text-text">{row.name}</h3>
        <span className="font-mono text-xs text-text-muted">{row.version}</span>
        {/*
          The standing, in its tone and at body size — it is the answer to "is this all right?",
          and it was an 11px word the same weight as everything around it. A `StateBadge` is the
          right shape and waits on a `workflow_standing` domain in the state map, which this page
          does not own.
        */}
        <span className="text-sm font-medium" style={{ color: `var(--tone-${tone}-fg)` }}>
          {row.standing}
        </span>
        {/*
          The overlay, counted. The canvas below draws each override; the count is what says so
          from the heading, before anybody has found the nodes in the picture.
        */}
        {overrides > 0 ? (
          <span className="text-xs text-text-muted">
            {overrides} {overrides === 1 ? "node" : "nodes"} overridden by this project
            {row.disabled_nodes > 0 ? `, ${row.disabled_nodes} switched off` : ""}
          </span>
        ) : null}
      </div>

      <p className="text-sm text-text">{standingSentence(row, now)}</p>
      {row.description !== null ? (
        <p className="max-w-prose text-sm text-text-muted">{row.description}</p>
      ) : null}

      {/*
        Both hashes, side by side, and only when they disagree. Serving them is what makes "this
        changed" a claim somebody can check instead of one they have to take on trust.
      */}
      {row.standing === "drifted" && row.origin_hash !== null ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 font-mono text-xs text-text-muted">
          <dt>pinned</dt>
          <dd>{shortHash(row.hash)}</dd>
          <dt>in the library</dt>
          <dd>{shortHash(row.origin_hash)}</dd>
        </dl>
      ) : null}

      {row.standing === "missing" ? (
        <p className="max-w-prose text-sm text-text-muted">
          The pin travelled with this repository and the bundle did not. It came from{" "}
          <span className="font-mono">{row.origin}</span> — which is what the pin records so that a
          new machine can be told what it is missing rather than quietly having no workflow.
        </p>
      ) : null}

      {row.owns.length > 0 ? (
        <p className="text-xs text-text-muted">
          Authors{" "}
          {row.owns.map((path, index) => (
            <span key={path}>
              {index > 0 ? ", " : null}
              <span className="font-mono">{path}</span>
            </span>
          ))}{" "}
          in this project.
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        {row.update_available !== null ? (
          <Button
            disabled={update.isPending}
            onClick={() =>
              update.mutate({ projectId, name: row.name, version: row.update_available ?? undefined })
            }
          >
            update to {row.update_available}
          </Button>
        ) : null}

        {row.standing === "drifted" ? (
          <Button
            disabled={update.isPending}
            onClick={() => update.mutate({ projectId, name: row.name, version: row.version })}
          >
            follow the library again
          </Button>
        ) : null}

        {row.standing === "ejected" ? (
          <>
            <Button aria-expanded={diffOpen} onClick={onToggleDiff}>
              {diffOpen ? "hide the diff" : "see the diff"}
            </Button>
            {/*
              Update after the diff and never before it: for an ejected workflow this REPLACES the
              copy, which is what update means and why the thing that shows what would be lost sits
              to its left. And behind an interlock, because it was the most destructive click on
              the page and fired on the first press — position is not confirmation.
            */}
            <ConfirmButton
              variant="danger"
              label="replace with the library's"
              confirmLabel="replace — this copy's edits are lost"
              disabled={update.isPending}
              onConfirm={() => update.mutate({ projectId, name: row.name })}
            />
          </>
        ) : guard.open ? null : (
          <Button id={ejectId} disabled={eject.isPending} onClick={guard.show}>
            eject
          </Button>
        )}

        {/*
          Also an interlock. Removing the pin takes this project's overrides with it — they live on
          the pin — and it happened on one click. An ejected copy is never deleted by this (the
          daemon refuses to delete somebody's files as a side effect of a list operation), so the
          armed label only names what is actually lost.
        */}
        <span className="ml-auto">
          <ConfirmButton
            variant="quiet"
            label="stop using it"
            confirmLabel={
              overrides > 0
                ? `stop — ${overrides} ${overrides === 1 ? "override goes" : "overrides go"} with the pin`
                : "stop — remove the pin"
            }
            disabled={forget.isPending}
            onConfirm={() => forget.mutate({ projectId, name: row.name })}
          />
        </span>
      </div>

      {guard.open ? (
        <EjectGuard
          pending={eject.isPending}
          originPath={originPath}
          onCancel={() => guard.close()}
          onEject={() => {
            guard.close();
            eject.mutate({ projectId, name: row.name });
          }}
          onEditOrigin={() => guard.close()}
        />
      ) : null}

      {/*
        A refusal is the middle weight, and a failure is not one. `RefusalNote` takes the núcleo's
        considered "no" with its code in mono; `REFUSALS` is this route's own sentences, because only
        the route knows which ceiling it hit. Anything that is not a refusal used to be dropped on
        the floor — the núcleo going away mid-click left the row showing its old state as if nothing
        had been asked.
      */}
      {failed === undefined ? null : isApiRefusal(failed.error) ? (
        <RefusalNote refusal={failed.error} sentences={REFUSALS} />
      ) : (
        <ErrorNote>The núcleo did not answer, so this may not have happened{said(failed.error)}</ErrorNote>
      )}

      {diffOpen ? <Diff projectId={projectId} name={row.name} /> : null}

      {/*
        The canvas, under the workflow it draws. Not a separate page and not a tab: the picture and
        the sentence about where the bundle stands are the same subject, and a surface that
        separated them would make somebody hold the standing in their head while looking at the
        graph.

        Only for a workflow this machine can actually produce a graph for. A `missing` pin has
        nothing to draw and the heading above already says what it is missing.
      */}
      {row.standing === "missing" ? null : (
        <WorkflowGraph
          projectId={projectId}
          name={row.name}
          originPath={originPath}
          ejected={row.standing === "ejected"}
        />
      )}
    </section>
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
 * a deliberate choice instead of the path of least resistance — and the sentence says what is
 * actually given up, which is future updates rather than anything visible today.
 *
 * Inline because §3.2 of the 2026-08-17 frontend spec is older and stronger than the convenience of
 * a dialog: a modal takes the page away to ask a question about something on it. It takes focus on
 * arrival and Escape closes it, which hands focus back to `eject` — see `useInlinePanel`.
 *
 * A well, not a bordered box: the classes are `Well`'s, written out, because the primitive does not
 * take the ref, role and key handler a focusable group needs.
 */
function EjectGuard({
  pending,
  originPath,
  onCancel,
  onEject,
  onEditOrigin,
}: {
  pending: boolean;
  /** Where the bundle is on this machine, or `null` when this machine does not have it. */
  originPath: string | null;
  onCancel: () => void;
  onEject: () => void;
  onEditOrigin: () => void;
}) {
  const self = useFocusOnMount<HTMLDivElement>();
  return (
    <div
      ref={self}
      role="group"
      aria-label="Eject or edit in the library"
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.stopPropagation();
        onCancel();
      }}
      className="ui-well ui-well-reading flex max-w-prose flex-col gap-2"
    >
      <p>
        Ejecting gives this project its own copy, and that copy stops receiving anything the library
        gets from then on. If the change is an improvement, the library is where it helps every
        project that uses this bundle.
      </p>
      <div className="flex flex-wrap items-center gap-3">
        <Button disabled={pending} onClick={onEject}>
          eject and edit
        </Button>
        <Button
          disabled={originPath === null}
          onClick={() => {
            if (originPath === null) return;
            // The library path, not the project's. `openInVscode` is the same door layer 2 uses for
            // everything this app does not author — and a bundle in the library is the clearest
            // case of that there is.
            void openInVscode(originPath, null);
            onEditOrigin();
          }}
        >
          edit in the library
        </Button>
        <Button variant="quiet" onClick={onCancel}>
          cancel
        </Button>
      </div>
    </div>
  );
}

/** The drift, file by file. */
function Diff({ projectId, name }: { projectId: string; name: string }) {
  const diff = useWorkflowDiff(projectId, name);

  if (diff.isError) {
    return isApiRefusal(diff.error) ? (
      <RefusalNote refusal={diff.error} sentences={REFUSALS} />
    ) : (
      <ErrorNote>The núcleo could not compare the two copies{said(diff.error)}</ErrorNote>
    );
  }
  if (diff.data === undefined) {
    return <p className="text-sm text-text-faint">Comparing…</p>;
  }
  if (diff.data.changes.length === 0) {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        Identical to {diff.data.origin_version} in the library, file for file — but still frozen: an
        ejected copy receives nothing new whether or not it has been edited.
      </p>
    );
  }

  /*
    A well, and it was a well with a border — which is the one mistake `Well` exists to stop, because
    an outline makes a box read as sitting ON the surface rather than as being cut into it.

    `reads` because the two halves have two authors. The sentence framing the comparison is prose
    somebody wrote; the file list under it is the núcleo's answer and carries its own mono, so the
    face keeps saying which is which instead of setting the whole box in the machine's.

    The change words are no longer coloured. `added` was Acting Green — "really executing, right
    now" — and `changed` was Held Ember, and neither is a state; the word already says which.
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
            <span className="w-14 shrink-0 text-text">{change.change}</span>
            <span className="text-text-muted">{change.path}</span>
          </li>
        ))}
      </ul>
      {diff.data.unchanged > 0 ? (
        <p className="mt-2 text-xs text-text-muted">
          {diff.data.unchanged} other {diff.data.unchanged === 1 ? "file" : "files"} identical.
        </p>
      ) : null}
    </Well>
  );
}

/* --------------------------------------------------------------- library -- */

/** What is on this machine, and what installing one would mean. */
function Library({
  projectId,
  installed,
  quietWhenEmpty,
}: {
  projectId: string;
  installed: Installed[];
  /** Nothing installed either: the empty state above already says all of this. */
  quietWhenEmpty: boolean;
}) {
  const library = useWorkflowLibrary();
  const install = useInstallWorkflow();

  if (library.isError) {
    // A refusal here was already said above when the project's own listing failed for the same
    // reason; saying it twice would be the page arguing with itself. A failure was not.
    return isApiRefusal(library.error) ? null : (
      <ErrorNote>The núcleo did not answer about the library{said(library.error)}</ErrorNote>
    );
  }
  if (library.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the library…</p>;
  }
  if (library.data.length === 0 && quietWhenEmpty) return null;

  const pinned = new Map(installed.map((row) => [row.name, row.version]));

  /*
    `Section` at rank 3, because this sits under the project mode's own heading rather than beside
    it. The region answers to its own heading instead of to "The library", which is a name nothing
    on screen ever said.
  */
  return (
    <Section label="On this machine" level={3}>
      {library.data.length === 0 ? (
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-2">
          <p className="max-w-prose text-sm text-text-muted">
            The library is empty. A bundle is a folder under{" "}
            <span className="font-mono">~/.nucleos/workflows/&lt;name&gt;/&lt;version&gt;/</span>{" "}
            with a <span className="font-mono">bundle.yaml</span> in it.
          </p>
          <LibraryPath />
        </div>
      ) : (
        /*
          A hairline-ruled column, and it was a stack of bordered boxes. Every bundle on the machine
          is one row of one list somebody scans to find the one they want; fifty of them as separate
          cards is a pile, and the eye stops reading a pile as a list.
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
      {install.isError ? (
        isApiRefusal(install.error) ? (
          <RefusalNote refusal={install.error} sentences={REFUSALS} />
        ) : (
          <ErrorNote>The núcleo did not answer, so nothing was installed{said(install.error)}</ErrorNote>
        )
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
    gesture pushed to the far end. The description takes what is left and gives way first, so a
    long one wraps under itself instead of pushing the button off the row.
  */
  return (
    <Row layout="line">
      <span className="text-sm text-text">{bundle.name}</span>
      <span className="font-mono text-xs text-text-muted">{bundle.version}</span>
      {bundle.description !== null ? (
        <span className="min-w-0 flex-1 text-xs text-text-muted">{bundle.description}</span>
      ) : null}
      {isPinned ? (
        <span className="ml-auto text-xs text-text-muted">used here</span>
      ) : (
        <span className="ml-auto">
          {/*
            One workflow per name, so installing a different version of one already in use is a
            change rather than an addition — and the button says which, because "install" over
            something that is working is the click somebody regrets.
          */}
          <Button disabled={pending} onClick={onInstall}>
            {pinnedVersion === null ? "use here" : `use ${bundle.version} instead`}
          </Button>
        </span>
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
  const now = useNow();

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
          <span className="text-xs text-text-muted">{standingSentence(row, now)}</span>
          {row.ejected_at !== null && row.standing === "ejected" ? (
            <span className="ml-auto text-xs text-text-faint">
              frozen {sinceText(row.ejected_at, now)}
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
