// §spec pilar-de-web
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The Web pillar: the local archive of pages already read, and the two doors
 * that add to it — reading one URL now, and searching outward through the
 * provider.
 *
 * **The quarantine is not a `{summary, facts, quotes}` structure** — that is
 * design §6.16's mistake. `content_md` under `trust: "quarantined"` is the
 * local model's own rendered prose: a literal banner line, a blank line, the
 * summary, then optional `- `-prefixed fact bullets, all as ONE string
 * (`core/src/web.rs:353-450,616-620`). There is no `quotes` field anywhere.
 * Every reader of `content_md` renders it as plain text with a badge beside
 * it — never parsed apart, never `dangerouslySetInnerHTML` — because the
 * banner and the bullets are exactly as trustworthy as the rest of it: a
 * stranger's claims, not a shape this shell gets to rely on.
 *
 * **`GET /web/pages` returns a bare array**, not `{ pages: [...] }`, and its
 * rows (`Hit`) carry far less than the full `Page` — no `requested_url`, no
 * `byline`, no `bytes`, no `extract_status`, no `trust_rule`. Those five
 * arrive only from `GET /web/pages/{id}`.
 *
 * **`provider: "unavailable"` is a state, not an error.** `POST /web/search`
 * turns the sidecar's own 503 into a normal 200 when no search provider is
 * configured (`web.rs:401-406`) — `cached` still comes back populated from
 * what this machine has already read. A page must switch on the literal, not
 * on whether the request threw.
 */

/** One row of the archive listing or a search of it — `web::Hit`. */
export interface Hit {
  id: number;
  final_url: string;
  host: string;
  title: string | null;
  snippet: string;
  trust_at_fetch: "raw" | "quarantined";
  fetched_at: string;
}

/** One page, in full — `web::Page`, `GET /web/pages/{id}`. */
export interface Page {
  id: number;
  requested_url: string;
  final_url: string;
  host: string;
  title: string | null;
  byline: string | null;
  /** Always the raw stored markdown here — the quarantine banner and summary, verbatim, when it applies. */
  content_md: string;
  extract_status: "article" | "fallback";
  /** A historical record of what happened at fetch time, not a permission. */
  trust_at_fetch: "raw" | "quarantined";
  trust_rule: string;
  bytes: number;
  fetched_at: string;
}

/** What `POST /web/read` hands back — `web::ReadView`. */
export interface ReadView {
  id: number;
  requested_url: string;
  final_url: string;
  host: string;
  title: string | null;
  trust: "raw" | "quarantined";
  trust_rule: string;
  extract_status: "article" | "fallback";
  content_md: string;
  from_cache: boolean;
  fetched_at: string;
}

/** One destination the search provider offered — `web_client::SearchResult`. */
export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
}

/** What `POST /web/search` hands back — `web::SearchView`. */
export interface SearchView {
  /** Local archive hits, first on purpose — what has already been read, before the internet. */
  cached: Hit[];
  provider: string;
  results: SearchResult[];
}

/**
 * The archive, unfiltered or searched — one route serves both
 * (`web.rs:495-509`): a whitespace-only `q` is read as absent, and `limit`
 * defaults to 50 either way.
 */
export function useWebPages(q?: string) {
  const trimmed = q?.trim();
  const query = trimmed === undefined || trimmed === "" ? "" : `?q=${encodeURIComponent(trimmed)}`;
  return useQuery({
    queryKey: keys.web.pages(trimmed === "" ? undefined : trimmed),
    queryFn: () => apiFetch<Hit[]>(`/web/pages${query}`),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** One archived page, in full — including the quarantine banner when it applies. Read-only scope. */
export function useWebPage(id: number | null) {
  return useQuery({
    queryKey: keys.web.page(id ?? -1),
    queryFn: () => apiFetch<Page>(`/web/pages/${id ?? -1}`),
    enabled: id !== null,
  });
}

/**
 * Read a URL now. Admin scope.
 *
 * **The page is stored even when the summary fails to arrive.** Under the
 * quarantine-unavailable family of refusals the daemon has already written
 * and indexed the row — only delivering a summary failed — so both success
 * and failure invalidate the archive listing rather than only success.
 */
export function useWebRead() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (url: string) =>
      apiFetch<ReadView>("/web/read", { method: "POST", body: JSON.stringify({ url }) }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.web.all });
    },
  });
}

/**
 * Search outward — the local archive first, whatever the provider answers
 * after it. Read-only scope, and this route writes nothing, so there is
 * nothing to invalidate.
 */
export function useWebSearch() {
  return useMutation({
    mutationFn: (input: { query: string; limit?: number }) =>
      apiFetch<SearchView>("/web/search", { method: "POST", body: JSON.stringify(input) }),
    retry: false,
  });
}
