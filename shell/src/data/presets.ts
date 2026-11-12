import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * Presets: a saved run request, by name.
 *
 * A preset is not a template for a run — it *is* the body `POST /runs` takes,
 * with a name attached. `POST /presets/{id}/run` goes through the same front
 * door as an ordinary create (`core/src/http.rs`, `run_preset` delegates to
 * `runs::create_run`), so it refuses for the same reasons and the shell says so
 * with the same copy.
 */

/** A saved run request as the daemon stores it. */
export interface Preset {
  id: number;
  name: string;
  prompt: string;
  project_id: string | null;
  cwd: string | null;
  mode: string;
  created_at: string;
  updated_at: string;
}

/**
 * What `POST /presets` and `PUT /presets/{id}` accept: the name, flattened over
 * the run request.
 *
 * `steerable` is deliberately absent. The daemon runs every preset with
 * `steerable: false` regardless of what is stored, because a preset records
 * *what* to run and not who may speak into it afterwards — so a field for it
 * here would be a control that silently does nothing.
 */
export interface PresetInput {
  name: string;
  prompt: string;
  project_id: string | null;
  cwd: string | null;
  mode: string;
}

/**
 * The saved requests.
 *
 * The slow cadence, and not the fast one: this list changes when a person edits
 * it and at no other time. Polling it every three seconds would be the shell
 * asking a question whose answer it already caused.
 */
export function usePresets() {
  return useQuery({
    queryKey: keys.presets.all,
    queryFn: () => apiFetch<Preset[]>("/presets"),
    refetchInterval: POLL.slow,
    placeholderData: keepPreviousData,
  });
}

/**
 * Save a request under a name.
 *
 * **409 means the name is taken** — the one refusal worth naming on this route,
 * and the reason a generic "conflict" would be useless here: the remedy is to
 * type a different name, which is not something a person guesses from the word
 * conflict.
 */
export function useCreatePreset() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: PresetInput) =>
      apiFetch<Preset>("/presets", { method: "POST", body: JSON.stringify(input) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.presets.all });
    },
  });
}

/** Rewrite a saved request. Refuses with the same 409 when the new name is taken. */
export function useUpdatePreset() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: number; input: PresetInput }) =>
      apiFetch<Preset>(`/presets/${id}`, { method: "PUT", body: JSON.stringify(input) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.presets.all });
    },
  });
}

/** Forget a saved request. `204`, and it takes no run with it. */
export function useDeletePreset() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<void>(`/presets/${id}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.presets.all });
    },
  });
}

/**
 * Start the run a preset describes.
 *
 * Answers with the new run's id, exactly as `POST /runs` does, because it *is*
 * `POST /runs` one call further in. The caller navigates to it — a preset that
 * started something and said nothing about where it went would be a button with
 * no visible effect.
 */
export function useRunPreset() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<{ id: number }>(`/presets/${id}/run`, { method: "POST" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.runs.all });
      void queryClient.invalidateQueries({ queryKey: keys.concurrency });
    },
  });
}
