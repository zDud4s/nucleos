import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * What the daemon says about one opt-in engine — `GET /engines/<engine>`.
 *
 * `warning` is the daemon's own text and the shell draws it as sent; it holds no copy.
 */
export interface EngineOptInState {
  enabled: boolean;
  acknowledged: boolean;
  warning: string;
  warning_version: number;
  installed: boolean;
}

/** `PUT /engines/<engine>` body: turning on needs `acknowledged: true`, turning off sends only `enabled`. */
export interface EngineOptInWrite {
  enabled: boolean;
  acknowledged?: boolean;
}

/** `PUT` answer. `takes_effect` is the daemon's sentence and only arrives with the write. */
export interface EngineOptInSaved {
  version: number;
  takes_effect: string;
}

export function useEngineOptIn(engine: string) {
  return useQuery({
    queryKey: keys.system.engine(engine),
    queryFn: () => apiFetch<EngineOptInState>(`/engines/${engine}`),
    retry: false,
  });
}

/**
 * `retry: false`: a refusal (`warning_not_acknowledged`, `kill_switch`, `stale`) is settled, and
 * retrying asks a question that has already been answered.
 */
export function useSetEngineOptIn(engine: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: EngineOptInWrite) =>
      apiFetch<EngineOptInSaved>(`/engines/${engine}`, {
        method: "PUT",
        body: JSON.stringify(body),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.engine(engine) });
      // The opt-in changes which models a chat may pick, so every model menu
      // (global, per-chat, front door and the grouped ones) goes stale too.
      void queryClient.invalidateQueries({ queryKey: keys.chats.models });
      void queryClient.invalidateQueries({ queryKey: keys.chats.modelGroups });
    },
  });
}
