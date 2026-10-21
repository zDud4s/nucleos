import { useCallback, useEffect, useRef, useState } from "react";
import { apiFetch, isApiRefusal } from "../data/client";
import { freshLines } from "./dedupe";
import { useFleetStore } from "./hooks";
import type { TermLine, TranscriptEntry, TranscriptPage } from "./types";

/** One page, newest when `before` is null. Both paths are literals for the route seam test. */
export async function fetchTranscriptPage(id: string, before: string | null): Promise<TranscriptPage> {
  const node = encodeURIComponent(id);
  return before === null
    ? await apiFetch<TranscriptPage>(`/fleet/nodes/${node}/transcript`)
    : await apiFetch<TranscriptPage>(`/fleet/nodes/${node}/transcript?before=${encodeURIComponent(before)}`);
}

/** An entry with a stable key, so an expanded tool stays open when an older page is put above it. */
export interface Row {
  key: number;
  entry: TranscriptEntry;
}
export type TranscriptStatus = "loading" | "ready" | "none" | "gone" | "failed";

const asEntry = (line: TermLine): TranscriptEntry => ({ kind: line.kind, text: line.text, detail: null });

/**
 * How long after the first page a stream batch is still compared with that page's tail. The hub
 * batches lines every 100 ms (contract `LINES_BATCH`), so a batch that was produced before the page
 * was read can reach the shell a few batches after it; two seconds covers that with room to spare,
 * and a repeat later than that is a genuine repeat.
 */
const DEDUPE_GRACE_MS = 2_000;

interface State {
  rows: Row[];
  before: string | null;
  status: TranscriptStatus;
  loadingEarlier: boolean;
  /** How many older pages have been put on top: the panel keys its scroll anchoring on it. */
  prepended: number;
}

/**
 * The paged transcript of one node plus its live tail. Mount it with `key={id}`: a different node
 * is a different hook instance, so there is no reset to write.
 *
 * Lines that arrive while the first page is in flight are buffered and then de-duplicated against
 * the page (`dedupe.ts`); batches that arrive in the two seconds after it are de-duplicated too,
 * and after that every batch from the stream is appended as it comes. If the page cannot be read
 * the buffered lines are shown anyway.
 */
export function useTranscript(id: string) {
  const store = useFleetStore();
  const [state, setState] = useState<State>({ rows: [], before: null, status: "loading", loadingEarlier: false, prepended: 0 });
  const serial = useRef({ low: 0, high: 0 });
  const buffer = useRef<TermLine[]>([]);
  const loaded = useRef(false);
  const firstPage = useRef<readonly TranscriptEntry[]>([]);
  const graceUntil = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const append = (entries: TranscriptEntry[]): Row[] => entries.map((entry) => ({ key: serial.current.high++, entry }));
    const stop = store.onLines(id, (lines) => {
      if (!loaded.current) {
        buffer.current.push(...lines);
        return;
      }
      // Right after the first page a batch can still be a line the page already contains.
      const fresh = Date.now() < graceUntil.current ? freshLines(firstPage.current, lines) : lines;
      if (fresh.length === 0) return;
      const rows = append(fresh.map(asEntry));
      setState((s) => ({ ...s, rows: [...s.rows, ...rows] }));
    });
    fetchTranscriptPage(id, null).then(
      (page) => {
        if (cancelled) return;
        const extra = freshLines(page.entries, buffer.current).map(asEntry);
        buffer.current = [];
        loaded.current = true;
        firstPage.current = page.entries;
        graceUntil.current = Date.now() + DEDUPE_GRACE_MS;
        // Keys are taken here, not inside the updater: an updater must stay pure.
        const rows = append([...page.entries, ...extra]);
        setState((s) => ({ ...s, rows, before: page.before, status: "ready" }));
      },
      (error: unknown) => {
        if (cancelled) return;
        loaded.current = true;
        // What arrived while the page was in flight is still worth showing under the note.
        const rows = append(buffer.current.map(asEntry));
        buffer.current = [];
        const status = isApiRefusal(error) ? (error.status === 409 ? "none" : error.status === 404 ? "gone" : "failed") : "failed";
        setState((s) => ({ ...s, rows: [...s.rows, ...rows], status }));
      },
    );
    return () => {
      cancelled = true;
      stop();
    };
  }, [store, id]);

  const { before, loadingEarlier } = state;
  const loadEarlier = useCallback(() => {
    if (before === null || loadingEarlier) return;
    setState((s) => ({ ...s, loadingEarlier: true }));
    fetchTranscriptPage(id, before).then(
      (page) => {
        const low = serial.current.low - page.entries.length;
        serial.current.low = low;
        const rows = page.entries.map((entry, i) => ({ key: low + i, entry }));
        setState((s) => ({ ...s, rows: [...rows, ...s.rows], before: page.before, loadingEarlier: false, prepended: s.prepended + 1 }));
      },
      () => setState((s) => ({ ...s, loadingEarlier: false })),
    );
  }, [id, before, loadingEarlier]);

  return { ...state, loadEarlier };
}
