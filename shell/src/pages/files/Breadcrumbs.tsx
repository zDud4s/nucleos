import { useLayoutEffect, useRef, useState } from "react";
import { Folder as FolderIcon } from "lucide-react";
import { pathSegments, pathUpTo } from "../../data/files";
import { Button } from "../../ui";

/* ---------------------------------------------------------- breadcrumbs -- */

export function Breadcrumbs({ path, onGo }: { path: string; onGo: (path: string) => void }) {
  const segments = pathSegments(path);
  const nav = useRef<HTMLElement>(null);
  // How many folders after "files" fold into "…". The path is one line and
  // never two: the far end goes first, since the near end is where you are and
  // the root is the way home. Measured, not guessed from a character count —
  // names are mono but the column is whatever the window leaves.
  const [folded, setFolded] = useState(0);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => setFolded(0), [path, width]);
  useLayoutEffect(() => {
    const element = nav.current;
    if (element === null) return;
    // The folder you are in may shrink to an ellipsis, which hides the overflow
    // from the bar's own measure: it counts as too long while it is cut.
    const here = element.querySelector<HTMLElement>(".fi-crumb-here");
    const over = element.scrollWidth > element.clientWidth || (here !== null && here.scrollWidth > here.clientWidth);
    if (over && folded < segments.length - 1) setFolded(folded + 1);
  }, [folded, segments.length]);
  useLayoutEffect(() => {
    const element = nav.current;
    if (element === null || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry !== undefined) setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const hidden = segments.slice(0, folded);
  // The folded folders open from "…" as the deepest of them, and name
  // themselves on hover: a jump one step past what is still written out.
  const lastHidden = pathUpTo(path, folded);

  // The folder you are in is a place, not a control: plain text carrying
  // `aria-current`, where it used to be a disabled button that dropped out of
  // the tab order without ever saying "you are here".
  return (
    // A path bar, not a trail of links floating over the toolbar: at the root
    // the lone "files" read as a stray word, and a bar is where the eye
    // already looks for "where am I" in every file manager.
    <nav ref={nav} className="fi-crumbs" aria-label="Folder path">
      <FolderIcon className="fi-kind" strokeWidth={1.5} aria-hidden="true" />
      {path === "" ? (
        <span className="fi-crumb fi-crumb-here" aria-current="location">
          files
        </span>
      ) : (
        <Button variant="link" data-drop-path="" onClick={() => onGo("")}>
          <span className="fi-crumb">files</span>
        </Button>
      )}
      {folded > 0 && (
        <Button
          variant="link"
          data-drop-path={lastHidden}
          title={hidden.join(" / ")}
          aria-label={`Go to ${hidden.join(" / ")}`}
          onClick={() => onGo(lastHidden)}
        >
          <span className="fi-crumb">/ …</span>
        </Button>
      )}
      {segments.map((segment, index) =>
        index < folded ? null : index === segments.length - 1 ? (
          <span
            key={`${segment}-${String(index)}`}
            className="fi-crumb fi-crumb-here"
            aria-current="location"
            title={segment}
          >
            / {segment}
          </span>
        ) : (
          <Button
            key={`${segment}-${String(index)}`}
            variant="link"
            data-drop-path={pathUpTo(path, index + 1)}
            onClick={() => onGo(pathUpTo(path, index + 1))}
          >
            <span className="fi-crumb">/ {segment}</span>
          </Button>
        ),
      )}
    </nav>
  );
}
