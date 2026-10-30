import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { SchemaNode } from "../settings/schema";
import { ApiRefusal, apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The núcleo's settings documents — one JSON document per pillar, each with a version and a history.
 *
 * The Rust struct is the schema: `GET /settings/{scope}/{name}` serves the document together with the
 * JSON Schema derived from it, and the shell draws the form from that. Nothing here knows what a pillar
 * contains.
 */
export type SettingKind = "machine" | "project";

/** One row of `GET /settings`. */
export interface SettingIndexRow {
  /** `machine`, or `project:<id>`. */
  scope: string;
  /** The area: `email`, `voice`, … for a machine; `autopilot` or `workflow_pins` for a project. */
  name: string;
  kind: SettingKind;
  /** A phrase completing "Takes effect ___." in the núcleo's own words. Rendered, never summarised. */
  takes_effect: string;
  /** Whether the document was ever written. `false` is "defaults only", not "empty". */
  present: boolean;
  version: number;
}

/** `GET /settings/{scope}/{name}`. */
export interface SettingDoc {
  /** The document, or the defaults when `version` is 0. Treated as `{}` when null. */
  body: unknown;
  /** 0 when never written. A write must carry the version it was read at. */
  version: number;
  schema: SchemaNode;
}

export interface SettingVersion {
  version: number;
  written_at: string;
  written_by: string;
  deleted: boolean;
}

export function settingPath(scope: string, name: string): string {
  return `/settings/${encodeURIComponent(scope)}/${encodeURIComponent(name)}`;
}

/** The index. With no scope it is the machine's documents (the caller filters by `scope`). */
export function useSettingsIndex(scope?: string) {
  return useQuery({
    queryKey: keys.settings.index(scope),
    queryFn: () =>
      apiFetch<SettingIndexRow[]>(
        scope === undefined ? "/settings" : `/settings?scope=${encodeURIComponent(scope)}`,
      ),
  });
}

export function useSettingDoc(scope: string, name: string) {
  return useQuery({
    queryKey: keys.settings.doc(scope, name),
    queryFn: () => apiFetch<SettingDoc>(settingPath(scope, name)),
  });
}

/** Listed only while `enabled` (the history panel is open). */
export function useSettingHistory(scope: string, name: string, enabled: boolean) {
  return useQuery({
    queryKey: keys.settings.history(scope, name),
    queryFn: () => apiFetch<SettingVersion[]>(`${settingPath(scope, name)}/history`),
    enabled,
  });
}

/**
 * Writes one document — `PUT /settings/{scope}/{name}` with the version it was read at.
 *
 * `retry: false`: the interesting failures are refusals (kill switch, not ours, invalid, stale) and a
 * refusal is settled. No optimistic update — the núcleo validates before it writes, so what the document
 * holds now is its answer and not ours to guess. Settled either way, every settings query is refetched.
 */
export function useWriteSetting(scope: string, name: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ body, version }: { body: unknown; version: number }) =>
      apiFetch<{ version: number }>(settingPath(scope, name), {
        method: "PUT",
        body: JSON.stringify({ body, version }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.settings.all });
    },
  });
}

/**
 * Restores an older version — `POST …/rollback/{target}`. It is a forward write: the núcleo stores the
 * old body as a NEW version, so nothing is lost and the history only grows. `current` is the version the
 * caller saw, for the same stale check a PUT has.
 */
export function useRollbackSetting(scope: string, name: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ target, current }: { target: number; current: number }) =>
      apiFetch<{ version: number }>(`${settingPath(scope, name)}/rollback/${target}`, {
        method: "POST",
        body: JSON.stringify({ version: current }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.settings.all });
    },
  });
}

/** Why a write did not land, in the five ways the núcleo can say no. */
export type WriteFailure =
  | { kind: "stale" }
  | { kind: "kill_switch" }
  | { kind: "not_ours" }
  | { kind: "invalid"; field: string | null; message: string }
  | { kind: "refused"; message: string }
  | { kind: "unreachable" };

function bodyString(body: unknown, key: string): string | null {
  if (typeof body !== "object" || body === null) return null;
  const value = (body as Record<string, unknown>)[key];
  return typeof value === "string" && value !== "" ? value : null;
}

/** By HTTP status, never by the body's wording: the words are the núcleo's to change. */
export function classifyWriteFailure(error: unknown): WriteFailure {
  if (!(error instanceof ApiRefusal)) return { kind: "unreachable" };
  switch (error.status) {
    case 409:
      return { kind: "stale" };
    case 423:
      return { kind: "kill_switch" };
    case 403:
      return { kind: "not_ours" };
    case 422:
      return {
        kind: "invalid",
        field: bodyString(error.body, "field"),
        message: bodyString(error.body, "message") ?? error.detail,
      };
    default:
      return { kind: "refused", message: error.detail !== "" ? error.detail : error.code };
  }
}
