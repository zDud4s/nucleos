import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * Which brain the distiller asks — `GET`/`POST /config/distiller`.
 *
 * `cloud` is the agent CLI and what the distiller has always used, `local` is the model on this
 * machine, `openrouter` the hosted one. The daemon reads the choice at the start of every pass, so
 * a change needs no restart; a choice this machine cannot serve leaves the queue waiting and never
 * falls back to the cloud.
 */
export type DistillerModel = "cloud" | "local" | "openrouter";

export const DISTILLER_MODELS: readonly DistillerModel[] = ["cloud", "local", "openrouter"];

function isDistillerModel(value: unknown): value is DistillerModel {
  return DISTILLER_MODELS.some((model) => model === value);
}

/**
 * The stored choice. A daemon that names nothing, or something this shell does not know, reads as
 * `cloud`: nobody has asked it to change anything, and the cloud is the default.
 */
export function useDistillerModel() {
  return useQuery({
    queryKey: keys.system.distiller,
    queryFn: async (): Promise<{ model?: string }> =>
      (await apiFetch<{ model?: string } | undefined>("/config/distiller")) ?? {},
    select: (reading): DistillerModel => (isDistillerModel(reading.model) ? reading.model : "cloud"),
  });
}

/**
 * Stores the choice — `POST /config/distiller`.
 *
 * `retry: false`, like every other mutation here: a refusal (`unknown_model`) is settled, and
 * retrying one asks a question that has already been answered. No optimistic write to the cache:
 * the answer to "what is stored now" is the daemon's.
 */
export function useSetDistillerModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (model: DistillerModel) =>
      apiFetch<{ model?: string }>("/config/distiller", {
        method: "POST",
        body: JSON.stringify({ model }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.distiller });
    },
  });
}
