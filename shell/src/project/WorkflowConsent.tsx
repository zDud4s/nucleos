// §spec motor-de-workflows
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import {
  commandDelta,
  useWorkflowConsent,
  type ConsentCommand,
  type WorkflowConsent,
} from "../data/workflows";
import { Button, ConfirmButton, ErrorNote, RefusalNote } from "../ui";
import { useFocusOnMount } from "./WorkflowGraph";

/**
 * The step between "I want this workflow" and the pin: every command it will run, as written.
 *
 * **A bundle is arbitrary code.** A command node runs a string with no hook and no classifier in
 * front of it, and installing is the one moment a person can read what that string is — after it,
 * the runs happen at night. So the list is the strings themselves, never a count and never a
 * summary, with the directory each one runs in.
 *
 * For an update the same list is set against what the project runs today and the commands that
 * moved come first: a `1.0.1` that adds `curl … | sh` to a node that used to run `pytest` is the
 * obvious way for this to go wrong, and the diff is the only defence.
 *
 * Nothing here judges a command. No allow-list, no sandbox, no scan of the text for anything that
 * looks bad: each of those would give the feel of a boundary without being one. The panel makes
 * the text visible and says what moved, and the person decides.
 *
 * Inline and never a modal, like the eject guard beside it: it takes focus on arrival and Escape
 * closes it.
 */
export interface WorkflowConsentPanelProps {
  projectId: string;
  name: string;
  version: string;
  /** What the confirming control says. */
  confirmLabel: string;
  /**
   * When set, confirming is a press-and-hold interlock instead of a click, and this is what the
   * hold says. For the one action here that throws something away.
   */
  holdLabel?: string;
  pending: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  /** This route's own sentences for the refusals it can hit. */
  sentences: Record<string, string>;
}

/** Where a command runs, in words. */
function whereText(cwd: string | null): string {
  return cwd === null || cwd === "" ? "in the project's folder" : `in ${cwd}`;
}

/**
 * One command, with the directory it runs in and whatever this project does to it.
 *
 * The command is in a `pre` and is never truncated or reflowed by this code: wrapping is the
 * browser's, and a line that is too long to see at a glance is exactly the one worth reading.
 */
function CommandItem({ line }: { line: ConsentCommand }) {
  const changed = line.change === "changed";
  return (
    <li className="flex flex-col gap-1">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="text-sm text-text">{line.label}</span>
        {line.label !== line.node ? (
          <span className="font-mono text-xs text-text-faint">{line.node}</span>
        ) : null}
        <span className="text-xs text-text-muted">{whereText(line.cwd)}</span>
        {line.timeout !== null ? (
          <span className="text-xs text-text-muted">stops after {line.timeout}</span>
        ) : null}
        {line.change === "changed" ? (
          <span className="text-xs font-medium text-text">changed</span>
        ) : null}
        {line.change === "new" ? <span className="text-xs font-medium text-text">new</span> : null}
        {line.disabled ? (
          <span className="text-xs text-text-muted">switched off in this project</span>
        ) : null}
      </div>
      {changed && line.previous_command !== undefined ? (
        <div className="flex flex-col gap-0.5">
          <span className="text-xs text-text-muted">was {whereText(line.previous_cwd ?? null)}:</span>
          <pre className="whitespace-pre-wrap break-all font-mono text-xs text-text-muted">
            {line.previous_command}
          </pre>
          <span className="text-xs text-text-muted">now:</span>
        </div>
      ) : null}
      <pre className="whitespace-pre-wrap break-all font-mono text-xs text-text">{line.command}</pre>
      {line.project_override !== undefined ? (
        <div className="flex flex-col gap-0.5">
          <span className="text-xs text-text-muted">
            this project runs this instead, through its own override:
          </span>
          <pre className="whitespace-pre-wrap break-all font-mono text-xs text-text">
            {line.project_override}
          </pre>
        </div>
      ) : null}
    </li>
  );
}

/** What the listing says before the commands do. */
function Lead({ consent }: { consent: WorkflowConsent }) {
  const count = consent.commands.length;
  const delta = commandDelta(consent);
  const moved = delta.changed.length + delta.added.length + delta.removed.length;
  const against =
    consent.baseline === null
      ? ""
      : consent.baseline_source === "project"
        ? ` this project's own copy (${consent.baseline})`
        : ` ${consent.baseline}`;

  return (
    <div className="flex flex-col gap-1">
      {count === 0 ? (
        <p>
          {consent.version} declares no command nodes, so it runs no program of its own. Agents
          still run on whatever model the graph names.
        </p>
      ) : (
        <p>
          {consent.version} runs {count} {count === 1 ? "command" : "commands"} in this project,
          with no hook and no classifier in front of {count === 1 ? "it" : "them"}. This is each
          one, exactly as the bundle writes it.
        </p>
      )}
      {consent.baseline === null ? null : moved === 0 ? (
        <p className="text-text-muted">
          No command differs from what runs now{against === "" ? "" : ` in${against}`}.
        </p>
      ) : (
        <p className="text-text">
          Against{against}:{" "}
          {[
            delta.changed.length > 0 ? `${delta.changed.length} changed` : null,
            delta.added.length > 0 ? `${delta.added.length} new` : null,
            delta.removed.length > 0 ? `${delta.removed.length} dropped` : null,
          ]
            .filter((part) => part !== null)
            .join(", ")}
          .
        </p>
      )}
    </div>
  );
}

/**
 * A listing that is already in hand: the lead, every command, and the dropped ones.
 *
 * The panel below reads it from `GET .../consent`; the new-project wizard has no pin to ask about
 * yet and gets it from the daemon's `consent_required` refusal instead.
 */
export function ConsentListing({ consent }: { consent: WorkflowConsent }) {
  // Moved commands first, because they are the ones that did not run yesterday.
  const rank = { changed: 0, new: 1, unchanged: 2 } as const;
  const ordered =
    consent.baseline === null
      ? consent.commands
      : // `sort` is stable, so the bundle's own order holds inside each group.
        [...consent.commands].sort((a, b) => rank[a.change] - rank[b.change]);

  return (
    <>
      <Lead consent={consent} />
      {ordered.length > 0 ? (
        <ul aria-label="Commands" className="flex flex-col gap-3">
          {ordered.map((line) => (
            <CommandItem key={line.node} line={line} />
          ))}
        </ul>
      ) : null}
      {consent.removed.length > 0 ? (
        <div className="flex flex-col gap-1">
          <p className="text-xs text-text-muted">No longer in {consent.version}:</p>
          <ul aria-label="Dropped commands" className="flex flex-col gap-2">
            {consent.removed.map((gone) => (
              <li key={gone.node} className="flex flex-col gap-0.5">
                <span className="text-xs text-text-muted">
                  {gone.node}, {whereText(gone.cwd)}
                </span>
                <pre className="whitespace-pre-wrap break-all font-mono text-xs text-text-muted">
                  {gone.command}
                </pre>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </>
  );
}

export function WorkflowConsentPanel({
  projectId,
  name,
  version,
  confirmLabel,
  holdLabel,
  pending,
  onConfirm,
  onCancel,
  sentences,
}: WorkflowConsentPanelProps) {
  const consent = useWorkflowConsent(projectId, name, version);
  const self = useFocusOnMount<HTMLDivElement>();

  // Nothing is agreed to until the list has been read: a control that fired before the commands
  // arrived would be a consent to text nobody saw.
  const ready = consent.data !== undefined && !pending;

  return (
    <div
      ref={self}
      role="group"
      aria-label={`Commands in ${name} ${version}`}
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        event.stopPropagation();
        onCancel();
      }}
      className="ui-well ui-well-reading flex max-w-prose flex-col gap-3"
    >
      {consent.isError ? (
        isApiRefusal(consent.error) ? (
          <RefusalNote refusal={consent.error} sentences={sentences} />
        ) : (
          <ErrorNote>
            The {DAEMON_NAME} gave no reply, so the commands could not be listed and nothing can be
            agreed to
            {consent.error instanceof Error && consent.error.message !== ""
              ? `: ${consent.error.message}`
              : "."}
          </ErrorNote>
        )
      ) : consent.data === undefined ? (
        <p className="text-sm text-text-faint">Reading the commands…</p>
      ) : (
        <ConsentListing consent={consent.data} />
      )}

      <div className="flex flex-wrap items-center gap-3">
        {holdLabel === undefined ? (
          <Button variant="primary" disabled={!ready} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        ) : (
          <ConfirmButton
            variant="danger"
            label={confirmLabel}
            confirmLabel={holdLabel}
            disabled={!ready}
            onConfirm={onConfirm}
          />
        )}
        <Button variant="quiet" onClick={onCancel}>
          cancel
        </Button>
      </div>
    </div>
  );
}
