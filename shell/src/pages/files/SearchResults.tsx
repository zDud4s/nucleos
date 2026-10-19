import { formatBytes, useFileSearch, type Hit } from "../../data/files";
import { Button, Count, Panel, Quiet, RelativeTime, Row, Rows, StaleNote } from "../../ui";
import { RefusalOrError, SEARCH_SENTENCES } from "./shared";

/* -------------------------------------------------------------- search -- */

export function SearchResults({
  result,
  onOpenFolder,
  onOpenFile,
  onReveal,
  onContextMenu,
}: {
  result: ReturnType<typeof useFileSearch>;
  onOpenFolder: (path: string) => void;
  /** The table's own open: a hit is the same file, and it used to download instead. */
  onOpenFile: (path: string) => void;
  /** Opens the hit's folder with the hit picked and the keyboard on it. */
  onReveal: (hit: Hit) => void;
  /** The table's row menu, for a hit: the same verbs on the same file. */
  onContextMenu: (hit: Hit, x: number, y: number, opener: HTMLElement | null) => void;
}) {
  const hits = result.data?.hits ?? [];
  return (
    <Panel
      variant="flat"
      title="Search results"
      aside={<Count n={result.data === undefined ? undefined : hits.length} noun="hit" />}
    >
      {/* Hits kept from an earlier search are marked as such, the way the table
          marks a stale listing — they may no longer be where they say. */}
      {result.isError && result.data !== undefined && <StaleNote dataUpdatedAt={result.dataUpdatedAt} />}
      {result.isError && <RefusalOrError error={result.error} sentences={SEARCH_SENTENCES} what="that search did not go through" />}
      {!result.isError && result.data === undefined && <p className="fi-loading">Searching…</p>}
      {/* The one-line absence, and `Quiet` rather than `.fi-loading`'s faint:
          here the sentence IS what the panel has to say, and an answer set
          fainter than the labels around it reads as failure rather than as
          emptiness. The wait above keeps the faint rung for the same reason. */}
      {result.data !== undefined && hits.length === 0 && <Quiet says="nothing under this folder matches." />}
      {result.data?.truncated === true && (
        <p className="fi-truncated" role="status">
          stopped early — this search hit the daemon's own ceiling before finishing the whole tree. Narrow it to
          see everything.
        </p>
      )}
      {hits.length > 0 && (
        // The row menu from anywhere on a hit, as on a row of the table: a hit
        // could be opened and nothing else, and "find what the agent wrote and
        // tidy it" ended at a list that could not tidy anything. The keyboard's
        // way in is Shift+F10 on the name, as in the table.
        // eslint-disable-next-line jsx-a11y/no-static-element-interactions
        <div
          onContextMenu={(event) => {
            const opener = (event.target as HTMLElement).closest("li")?.querySelector<HTMLElement>("[data-hit]") ?? null;
            const hit = hits[Number(opener?.dataset.hit ?? -1)];
            if (hit === undefined) return;
            event.preventDefault();
            onContextMenu(hit, event.clientX, event.clientY, opener);
          }}
        >
          <Rows label="Search hits">
            {hits.map((hit, index) => (
              <Row key={hit.path} layout="line">
                {/* The table's rule: a folder on one click, a file on a double-click or Enter. */}
                <Button
                  variant="link"
                  data-hit={index}
                  title={hit.is_dir ? undefined : "Double-click to open in Windows"}
                  onKeyDown={(event) => {
                    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
                      event.preventDefault();
                      const rect = event.currentTarget.getBoundingClientRect();
                      onContextMenu(hit, rect.left, rect.bottom, event.currentTarget);
                    }
                  }}
                  onClick={(event) => {
                    if (hit.is_dir) onOpenFolder(hit.path);
                    else if (event.detail === 0) onOpenFile(hit.path);
                  }}
                  onDoubleClick={() => {
                    if (!hit.is_dir) onOpenFile(hit.path);
                  }}
                >
                  <span className="fi-entry-name">
                    {hit.name}
                    {hit.is_dir ? "/" : ""}
                  </span>
                </Button>
                <span className="fi-hit-path">{hit.path}</span>
                <span className="fi-hit-size">{hit.is_dir ? "" : formatBytes(hit.size_bytes)}</span>
                {/* The column the table has and the hits did not: which of them
                    is the one just written is most of why a person searches. */}
                <span className="fi-hit-when">
                  {hit.modified === null ? <span title="the disk did not say when">—</span> : <RelativeTime at={hit.modified} />}
                </span>
                {!hit.is_dir && (
                  <Button variant="ghost" aria-label={`Show ${hit.name} in its folder`} onClick={() => onReveal(hit)}>
                    Show in folder
                  </Button>
                )}
              </Row>
            ))}
          </Rows>
        </div>
      )}
    </Panel>
  );
}
