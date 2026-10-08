import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * Which Ollama model embeds knowledge rows — `GET`/`POST /config/embedding`.
 *
 * A save takes effect at once, with no restart. Rows embedded by the previous model are not
 * compared against the new one; the daemon re-embeds them in the background.
 */
export function useEmbeddingModel() {
  return useQuery({
    queryKey: keys.system.embedding,
    queryFn: async (): Promise<{ model?: string }> =>
      (await apiFetch<{ model?: string } | undefined>("/config/embedding")) ?? {},
    select: (reading): string => (typeof reading.model === "string" ? reading.model : ""),
  });
}

/** Stores the choice — `POST /config/embedding`. A refusal (`unknown_model`) is settled, so no retry. */
export function useSetEmbeddingModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (model: string) =>
      apiFetch<{ model?: string }>("/config/embedding", {
        method: "POST",
        body: JSON.stringify({ model }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.embedding });
    },
  });
}
