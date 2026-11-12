import { useEffect, useMemo, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * Global search: the palette asking every place that already keeps text.
 *
 * No route of its own at the daemon. Each source is an endpoint that exists and that a page
 * already uses; this file only asks them all at once, five rows each, and says where choosing a
 * row goes. If six requests per pause in the typing turns out to cost too much, the answer is an
 * aggregate route in the núcleo, not a smarter client.
 */

/** Characters before anything is asked — below this a query matches half the database. */
export const GLOBAL_SEARCH_MIN = 3;
/** Rows kept per source. */
export const GLOBAL_SEARCH_PER_SOURCE = 5;
/** Longest excerpt shown. Text, never markup. */
export const EXCERPT_MAX = 120;

export type SearchSourceId = "said" | "runs" | "feed" | "mail" | "web" | "notes";

/** One row of the palette, as plain data: the component draws it and decides what Enter does. */
export interface SearchHit {
  /** Unique within the whole palette — cmdk keys its selection by it. */
  id: string;
  title: string;
  /** A second small line (sender, host, status), when there is one. */
  detail?: string;
  /** The excerpt: whitespace folded, cut to {@link EXCERPT_MAX}. */
  text: string;
  /** The route Enter opens. */
  to: string;
  search?: Record<string, string>;
}

interface SearchSource<Row> {
  id: SearchSourceId;
  heading: string;
  /** The key under `keys.search`. */
  key: (q: string) => readonly unknown[];
  path: (encoded: string) => string;
  /** What the daemon sent, cut to the rows to show; anything not a list is no rows. */
  rows: (data: unknown) => Row[];
  hit: (row: Row, q: string) => SearchHit;
}

/** Fold whitespace and cut. Never HTML: the caller renders this as a text node. */
export function excerpt(text: string | null | undefined): string {
  const flat = (text ?? "").replace(/\s+/g, " ").trim();
  return flat.length <= EXCERPT_MAX ? flat : `${flat.slice(0, EXCERPT_MAX - 1).trimEnd()}…`;
}

function list<Row>(data: unknown): Row[] {
  return Array.isArray(data) ? (data.slice(0, GLOBAL_SEARCH_PER_SOURCE) as Row[]) : [];
}

interface SaidRow {
  chat_id: string;
  title: string | null;
  turn_id: number;
  excerpt: string;
}
interface RunRow {
  id: number;
  status?: string;
  prompt_excerpt?: string | null;
  chat_title?: string | null;
}
interface FeedRow {
  id: number;
  kind?: string;
  summary?: string | null;
}
interface MailRow {
  id: number;
  from_addr?: string;
  from_name?: string | null;
  subject?: string | null;
  triage_summary?: string | null;
}
interface WebRow {
  id: number;
  title?: string | null;
  host?: string;
  snippet?: string | null;
}
interface NoteRow {
  id: number;
  text?: string;
}

function source<Row>(definition: SearchSource<Row>): SearchSource<Row> {
  return definition;
}

/** Heading order is the order the groups appear in the palette. */
const sources = [
  source<SaidRow>({
    id: "said",
    heading: "Said in a conversation",
    key: keys.search.said,
    path: (q) => `/assistant/search?q=${q}`,
    rows: list,
    hit: (row) => ({
      id: `search-said-${row.turn_id}`,
      title: row.title ?? "New conversation",
      text: excerpt(row.excerpt),
      to: `/chats/${row.chat_id}`,
    }),
  }),
  source<RunRow>({
    id: "runs",
    heading: "Runs",
    key: keys.search.runs,
    path: (q) => `/runs?q=${q}&limit=${GLOBAL_SEARCH_PER_SOURCE}`,
    rows: list,
    hit: (row) => ({
      id: `search-run-${row.id}`,
      title: `Run #${row.id}`,
      detail: row.status,
      text: excerpt(row.prompt_excerpt ?? row.chat_title),
      to: `/runs/${row.id}`,
    }),
  }),
  source<FeedRow>({
    id: "feed",
    heading: "Feed",
    key: keys.search.feed,
    path: (q) => `/feed?scope=all&q=${q}&limit=${GLOBAL_SEARCH_PER_SOURCE}`,
    rows: list,
    hit: (row, q) => ({
      id: `search-feed-${row.id}`,
      title: row.kind ?? "Feed",
      text: excerpt(row.summary),
      // The feed has no page per line; the page that lists them, narrowed to what was typed.
      to: "/feed",
      search: { q },
    }),
  }),
  source<MailRow>({
    id: "mail",
    heading: "Mail",
    key: keys.search.mail,
    // The queue route has no `limit`: it returns up to 200, and the cut is made here.
    path: (q) => `/email/queue?q=${q}`,
    rows: list,
    hit: (row) => ({
      id: `search-mail-${row.id}`,
      title: row.subject?.trim() ? row.subject : "(no subject)",
      detail: row.from_name?.trim() ? row.from_name : row.from_addr,
      text: excerpt(row.triage_summary),
      to: `/mail/${row.id}`,
    }),
  }),
  source<WebRow>({
    id: "web",
    heading: "Saved web pages",
    key: keys.search.web,
    path: (q) => `/web/pages?q=${q}&limit=${GLOBAL_SEARCH_PER_SOURCE}`,
    rows: list,
    hit: (row) => ({
      id: `search-web-${row.id}`,
      title: row.title?.trim() ? row.title : (row.host ?? "Saved page"),
      detail: row.host,
      text: excerpt(row.snippet),
      to: `/web/pages/${row.id}`,
    }),
  }),
  source<NoteRow>({
    id: "notes",
    heading: "Notes",
    key: keys.search.notes,
    path: (q) => `/owner-notes/search?q=${q}`,
    rows: list,
    hit: (row) => ({
      id: `search-note-${row.id}`,
      title: "Note",
      text: excerpt(row.text),
      // Brain opens on the notes themselves; it takes no note id to land on.
      to: "/brain",
    }),
  }),
] as const;

/** The sources, in palette order. */
export const SEARCH_SOURCES: readonly { id: SearchSourceId; heading: string }[] = sources;

/** What one source found: absent from the result altogether when it found nothing or failed. */
export interface SearchGroup {
  id: SearchSourceId;
  heading: string;
  hits: SearchHit[];
}

/**
 * Hold a value back until it has stopped changing for `ms`.
 *
 * For a query typed into the palette: each keystroke would otherwise be six requests.
 */
export function useDebounced<T>(value: T, ms: number): T {
  const [held, setHeld] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setHeld(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return held;
}

export interface GlobalSearchOptions {
  /** Sources not to ask — Chats already shows the conversation hits itself. */
  skip?: readonly SearchSourceId[];
}

/**
 * Ask every source for `q`, and return the ones that answered with something.
 *
 * A source that fails (401, 500, a sidecar that is down) is a source with no rows: the others
 * are untouched, and nothing here throws. Order within a source is the daemon's.
 */
export function useGlobalSearch(q: string, options: GlobalSearchOptions = {}): SearchGroup[] {
  const trimmed = q.trim();
  const asking = trimmed.length >= GLOBAL_SEARCH_MIN;
  const skip = options.skip ?? [];
  const active = sources.filter((entry) => !skip.includes(entry.id));
  const encoded = encodeURIComponent(trimmed);

  const answers = useQueries({
    queries: active.map((entry) => ({
      queryKey: entry.key(trimmed),
      queryFn: () => apiFetch<unknown>(entry.path(encoded)),
      enabled: asking,
      retry: false,
      staleTime: 30_000,
    })),
    combine: (results) => results.map((result) => result.data),
  });

  // `answers` keeps its identity while no source's data changed, so this does not rebuild the
  // groups (and re-register them in the palette) on an unrelated render.
  return useMemo(() => {
    if (!asking) return [];
    const groups: SearchGroup[] = [];
    active.forEach((entry, at) => {
      const hits = (entry.rows(answers[at]) as never[]).map((row) =>
        (entry.hit as (row: never, q: string) => SearchHit)(row, trimmed),
      );
      if (hits.length > 0) groups.push({ id: entry.id, heading: entry.heading, hits });
    });
    return groups;
    // `active` is derived from `skip` by value; the answers and the query are what change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [answers, asking, trimmed, skip.join(",")]);
}
