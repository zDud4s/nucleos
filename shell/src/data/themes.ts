import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText } from "./client";
import { keys } from "./keys";
import type { GraphTheme } from "../brain/graph-types";

/**
 * The themes, as hooks — `core/src/http/themes.rs` over `/themes`.
 *
 * | what                 | route                          |
 * |----------------------|--------------------------------|
 * | list (counts only)   | `GET /themes?state=`           |
 * | one, with members    | `GET /themes/{id}`             |
 * | accept (optional name)| `POST /themes/{id}/accept`    |
 * | refuse               | `POST /themes/{id}/refuse`     |
 * | force a synthesis    | `POST /themes/{id}/synthesise` |
 *
 * The list never carries the synthesis; the detail shows it only for an accepted theme.
 */

export type ThemeState = "proposed" | "accepted" | "refused" | "merged";

export interface ThemeRow {
  id: number;
  name: string;
  state: ThemeState;
  merged_into: number | null;
  /** Live (not excluded) members. */
  members: number;
  created_at: string;
}

export interface ThemeMember {
  kind: string;
  id: number;
  origin: string;
  excluded: boolean;
  label: string;
  missing: boolean;
}

export interface ThemeDetail {
  id: number;
  name: string;
  state: ThemeState;
  merged_into: number | null;
  synthesis: string | null;
  created_at: string;
  decided_at: string | null;
  members: ThemeMember[];
}

export function useThemes(state?: ThemeState) {
  return useQuery({
    queryKey: keys.themes.list(state ?? "all"),
    queryFn: () => apiFetch<ThemeRow[]>(state === undefined ? "/themes" : `/themes?state=${state}`),
  });
}

export function useTheme(id: number | null) {
  return useQuery({
    queryKey: keys.themes.detail(id ?? -1),
    queryFn: () => apiFetch<ThemeDetail>(`/themes/${id}`),
    enabled: id !== null,
  });
}

/** A detail as the graph wants it: only live note and knowledge members. */
export function toGraphTheme(detail: ThemeDetail): GraphTheme | null {
  if (detail.state !== "proposed" && detail.state !== "accepted") return null;
  const members: GraphTheme["members"] = [];
  for (const m of detail.members) {
    if (m.excluded || m.missing) continue;
    if (m.kind === "note" || m.kind === "knowledge") members.push({ kind: m.kind, id: m.id });
  }
  return { id: detail.id, name: detail.name, state: detail.state, members };
}

const EMPTY: GraphTheme[] = [];

/** Every live theme with its members, for `buildModel`. Empty while loading or when the list failed. */
export function useGraphThemes(): GraphTheme[] {
  const list = useThemes();
  const live = (list.data ?? []).filter((t) => t.state === "proposed" || t.state === "accepted");
  return useQueries({
    queries: live.map((t) => ({
      queryKey: keys.themes.detail(t.id),
      queryFn: () => apiFetch<ThemeDetail>(`/themes/${t.id}`),
    })),
    combine: (results) => {
      const out: GraphTheme[] = [];
      for (const r of results) {
        const theme = r.data === undefined ? null : toGraphTheme(r.data);
        if (theme !== null) out.push(theme);
      }
      return out.length === 0 ? EMPTY : out;
    },
  });
}

function useThemeMutation<Input, Result>(mutationFn: (input: Input) => Promise<Result>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A decision is made once; a retried accept would only be refused.
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.themes.all });
    },
  });
}

export function useAcceptTheme() {
  return useThemeMutation(({ id, name }: { id: number; name?: string }) =>
    // The route answers a bare 200: no JSON to read.
    apiText(`/themes/${id}/accept`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(name === undefined ? {} : { name }),
    }),
  );
}

export function useRefuseTheme() {
  return useThemeMutation((id: number) =>
    apiText(`/themes/${id}/refuse`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    }),
  );
}

/** Forces a synthesis now: 409 not accepted, 429 within the hour, 503 when the distiller model cannot be served. */
export function useSynthesiseTheme() {
  return useThemeMutation((id: number) =>
    apiFetch<{ synthesis: string | null }>(`/themes/${id}/synthesise`, { method: "POST" }),
  );
}
