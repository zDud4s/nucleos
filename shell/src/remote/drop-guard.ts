/**
 * Keeps a file dropped on a browser tab from navigating the tab to that file.
 *
 * Only a drop zone takes a drop (the Files page's `.fi-body`); anywhere else the browser's default
 * is to open the file and leave the app. The guard cancels that default and nothing else: it does
 * not stop propagation, so a zone's own handlers and the context refs still run. The desktop host
 * has its own drop events and never installs this.
 *
 * Returns the remover.
 */
export function installDropGuard(target: Window | Document = window): () => void {
  const prevent = (event: Event) => event.preventDefault();
  target.addEventListener("dragover", prevent);
  target.addEventListener("drop", prevent);
  return () => {
    target.removeEventListener("dragover", prevent);
    target.removeEventListener("drop", prevent);
  };
}
