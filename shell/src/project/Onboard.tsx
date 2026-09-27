import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useOnboard, useOnboarding } from "../data/onboarding";
import { Button, ErrorNote, Field, RefusalNote } from "../ui";

/**
 * Onboarding one project: what is in its folder, the gate command to confirm, and the button.
 *
 * The mode door asks this of a project before it will watch it or let it act, beside the
 * classifier hook, and the Autopilot page offers it where that refusal lands. Small on purpose:
 * the harnesses as found, the gate as a field that starts from what the project already names
 * (or, failing that, what the núcleo proposes), and one confirmation. A blank gate is a real
 * answer — nobody confirmed one — and it leaves the project's rules as they were.
 *
 * `root` is the folder, for a project with none on record; empty uses the one on record.
 */
export function OnboardPanel({
  projectId,
  root,
  onDone,
}: {
  projectId: string;
  root: string;
  onDone?: () => void;
}) {
  const reading = useOnboarding(projectId, root);
  const onboard = useOnboard();
  const [gate, setGate] = useState<string | null>(null);

  const found = reading.data;

  if (reading.isError) {
    return isApiRefusal(reading.error) ? (
      <RefusalNote refusal={reading.error} sentences={ONBOARD_SENTENCES} />
    ) : (
      <ErrorNote>the núcleo did not answer about this project's folder.</ErrorNote>
    );
  }
  if (found === undefined) return <p className="text-sm text-text-muted">reading the folder…</p>;

  const proposal = found.proposed_gate;
  // Untouched, the field reads the gate the project already names, then the proposal; once typed
  // in, it is what was typed — a refetch never puts a proposal back over somebody's words.
  const shown = gate ?? found.configured_gate ?? proposal?.command ?? "";
  const source =
    found.configured_gate !== null
      ? "the gate this project's rules already name"
      : proposal === null
        ? "nothing in the folder suggests one — leave it blank for none"
        : `proposed from ${proposal.source}`;

  return (
    <div className="flex flex-col gap-3" aria-label={`Onboard ${projectId}`} role="group">
      <p className="max-w-(--measure) text-sm text-text">
        Onboarding <span className="font-mono">{projectId}</span> installs the classifier hook in{" "}
        <span className="font-mono">{found.root}</span>, stores the gate below, and records that it
        was done in <span className="font-mono">{found.marker_path}</span>.
      </p>
      <p className="text-sm text-text-muted">
        {found.harnesses.length === 0
          ? "No way of working found in the folder — which is fine; none is needed."
          : `Found: ${found.harnesses.map((harness) => harness.path).join(", ")}.`}
      </p>
      <Field label="Gate command" helper={source}>
        <input
          value={shown}
          spellCheck={false}
          onChange={(event) => setGate(event.target.value)}
          className="w-full font-mono"
        />
      </Field>
      <div className="flex flex-wrap items-center gap-3">
        <Button
          intent="go"
          disabled={onboard.isPending}
          aria-busy={onboard.isPending}
          onClick={() =>
            onboard.mutate(
              { projectId, projectRoot: root === "" ? null : root, gateCommand: shown },
              { onSuccess: () => onDone?.() },
            )
          }
        >
          {onboard.isPending ? "onboarding…" : found.onboarded === null ? "onboard it" : "onboard it again"}
        </Button>
        {onboard.isSuccess ? <span className="text-sm text-text-muted">onboarded</span> : null}
      </div>
      {onboard.isError ? (
        isApiRefusal(onboard.error) ? (
          <RefusalNote refusal={onboard.error} sentences={ONBOARD_SENTENCES} />
        ) : (
          <ErrorNote>the núcleo did not answer while onboarding.</ErrorNote>
        )
      ) : null}
    </div>
  );
}

/**
 * The onboarding door's named refusals. The three that carry the daemon's reason (`invalid_gate`,
 * `rules_unreadable`, `hook_unwritable`) are left to it — its words say which line.
 */
const ONBOARD_SENTENCES: Record<string, string> = {
  no_project_root: "this project has no folder on record — give it one first.",
  no_such_folder: "there is nothing at that path.",
  not_a_folder: "that path is a file, not a folder.",
  not_absolute: "the folder has to be an absolute path.",
  bad_project_id: "this project's id cannot name a directory, so it cannot be onboarded.",
  no_machine_root: "this machine has no home directory for the núcleo to keep the record in.",
};
