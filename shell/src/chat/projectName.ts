/**
 * The last segment of a working directory — what a person calls the project.
 *
 * Shown instead of the whole path because the list is a narrow column and the interesting part of
 * `C:\Projects\nucleos-canvas` is the end of it. Handles both separators: the daemon reads these
 * paths out of the CLI's transcripts, which record whatever the machine that wrote them used.
 */
export function projectName(cwd: string): string {
  const parts = cwd.split(/[\\/]/).filter((part) => part !== "");
  return parts.length === 0 ? cwd : parts[parts.length - 1];
}
