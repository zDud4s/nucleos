/**
 * "What is this?" — a review surface, not an IDE.
 *
 * The distinction is about the *subject*. A file browser's subject is the
 * repository, and it opens on four thousand files; this one's subject is the
 * **run**, so it opens on what changed and says how many files nobody touched.
 * The button that closes the screen is not *save* — it is *approve and land*.
 *
 * Where code gets changed is VS Code, on the same machine and the same
 * repository, reached at the file and the line through `lib/vscode.ts`. That
 * seam is already built; the columns around it are not.
 *
 * Not embedded, and the argument is not about the desktop: this núcleo approves
 * work from Telegram and e-mail too, where an editor is useless and a good diff
 * reader is exactly right.
 */

export interface ModeCodigoProps {
  projectId: string;
}

export function ModeCodigo({ projectId }: ModeCodigoProps) {
  return (
    <div className="rounded-lg border border-dashed border-border bg-surface-sunken p-6">
      <p className="font-display text-lg text-text-muted">
        The review surface for {projectId} is not built yet.
      </p>
      <p className="mt-2 max-w-prose text-sm text-text-faint">
        It arrives with the núcleo's blame route, alongside the changed-file list and the diff
        readers that already exist. Until then a run's diff is on its own page, under Runs.
      </p>
    </div>
  );
}
