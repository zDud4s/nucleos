/**
 * "How does this get developed?"
 *
 * A workflow is a graph of how work moves through a project, and this núcleo
 * already has one — it is just implicit and spread across three files. `.ai/`
 * holds the sequence in prose, the model assignments in YAML, and the
 * conditional edges in a numbered list of rules ("review runs when Risk is
 * elevated OR Size is medium/large"). It already mixes four kinds of step
 * without ever saying so. This mode does not invent a vocabulary; it names what
 * is there.
 *
 * The canvas lands once a workflow is a thing the núcleo can hold: a library, a
 * pin with an origin and a hash, and the project's overlay on top of it.
 */

export interface ModeWorkflowsProps {
  projectId: string;
}

export function ModeWorkflows({ projectId }: ModeWorkflowsProps) {
  return (
    <div className="rounded-lg border border-dashed border-border bg-surface-sunken p-6">
      <p className="font-display text-lg text-text-muted">
        No workflow is installed in {projectId}, and nothing can install one yet.
      </p>
      <p className="mt-2 max-w-prose text-sm text-text-faint">
        The library, the pin and the overlay come first; the graph editor is drawn on top of them.
        The harness in this repository's own <span className="font-mono">.ai/</span> is the first
        bundle it will have to recognise — a project that already works is not asked to be recreated
        in an editor.
      </p>
    </div>
  );
}
