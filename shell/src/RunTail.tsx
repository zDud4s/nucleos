import { useEffect, useRef, useState } from "react";

import { getRunTail } from "./api";

interface RunTailProps {
  token: string;
  runId: number;
  /** Whether the run is still moving, as the view around this one last read it. */
  live: boolean;
}

/** Where the last read said this run's output is. */
type Source = "reading" | "live" | "recorded" | "unreachable";

/** How close to the bottom still counts as watching the end, in pixels. */
const PINNED_WITHIN = 24;

/**
 * A run's output while it is still being written.
 *
 * Reads `GET /runs/{id}/tail?since=` every 3 seconds and appends what comes back, which is why it
 * keeps its own copy of the text rather than re-rendering a field of the run: the daemon sends the
 * new bytes only, and the whole point is not to re-fetch a transcript that grows all afternoon.
 *
 * The tail is held in the daemon's memory and disappears with the run, so ABSENT and EMPTY are
 * different answers and this component never merges them. A 204 means the live tail is gone — the
 * run finished, or a previous daemon started it — and the output is in `runs.stdout`, which the
 * detail view around this one already shows. A failed read means neither: it is the one state where
 * what is on screen is all that can be said, and saying "recorded" there would send someone looking
 * for lines the run has not written yet.
 */
export default function RunTail({ token, runId, live }: RunTailProps) {
  const [text, setText] = useState("");
  const [source, setSource] = useState<Source>("reading");
  /** The daemon's `next`, in bytes. Never derived from `text.length`. */
  const since = useRef(0);
  const inFlight = useRef(false);
  const stream = useRef<HTMLPreElement>(null);
  /** Whether the reader is watching the end. False the moment they scroll up to read. */
  const pinned = useRef(true);

  // Opening a different run starts a different transcript. Keyed on `runId` alone: folding this
  // into the poll below would wipe the text every time `live` flips, which is exactly the moment a
  // reader is looking at the last thing the run said.
  useEffect(() => {
    since.current = 0;
    setText("");
    setSource("reading");
  }, [runId]);

  useEffect(() => {
    let cancelled = false;
    const read = async (background = false) => {
      // One read at a time, and only the background ones give way: the read that runs when `live`
      // goes false is the one that finds the tail gone, and skipping it would leave the panel
      // claiming to be live over a run that had ended.
      if (background && inFlight.current) return;
      inFlight.current = true;
      try {
        const chunk = await getRunTail(token, runId, since.current);
        if (cancelled) return;
        if (chunk === null) {
          setSource("unreachable");
          return;
        }
        if (chunk === "recorded") {
          setSource("recorded");
          return;
        }
        since.current = chunk.next;
        if (chunk.text !== "") setText((current) => current + chunk.text);
        setSource("live");
      } finally {
        inFlight.current = false;
      }
    };

    void read();
    if (!live) {
      return () => {
        cancelled = true;
      };
    }
    const id = setInterval(() => void read(true), 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [live, runId, token]);

  useEffect(() => {
    const node = stream.current;
    if (node !== null && pinned.current) node.scrollTop = node.scrollHeight;
  }, [text]);

  // Nothing read yet, and nothing to say about it either.
  if (text === "" && source === "reading") return null;
  // A finished run with no live tail: the `output` block below is the answer, and a line pointing
  // at it would be chrome under every run in the history.
  if (text === "" && source === "recorded" && !live) return null;

  return (
    <div className="rd-tail">
      <p className="rd-tail__state">{note(source, text !== "")}</p>
      {text !== "" && (
        // Text, never markup: this is a model's output and a subprocess's, in the same way an email
        // body is.
        <pre
          className="rd-tail__stream rd-stream"
          ref={stream}
          onScroll={() => {
            const node = stream.current;
            if (node === null) return;
            pinned.current =
              node.scrollHeight - node.scrollTop - node.clientHeight < PINNED_WITHIN;
          }}
        >
          {text}
        </pre>
      )}
    </div>
  );
}

/** What the panel can honestly claim about what it is showing. */
function note(source: Source, hasText: boolean): string {
  switch (source) {
    case "live":
      return "live — this is being written now";
    case "recorded":
      return hasText
        ? "the live tail ended here; the whole output is recorded below"
        : "no live output: this run's output is recorded, not streaming";
    case "unreachable":
      return "the daemon did not answer — this is what had arrived";
    default:
      return "reading…";
  }
}
