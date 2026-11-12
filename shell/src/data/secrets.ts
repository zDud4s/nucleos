import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The credentials this machine holds, by whether they are set and never by what they are.
 *
 * They live in the OS keyring, not in the settings database, and keep their own routes. A settings
 * document can only name a credential (the `secret-ref` widget); the value is write-only, here.
 */

/**
 * One credential this machine holds — by whether it is set, never by what it is.
 *
 * There is no route that serves a value and there is no field here for one. The
 * núcleo enforces it a layer down: its `SecretStore` has no method that returns
 * a secret, so a future handler cannot serve one by accident.
 */
export interface MachineSecret {
  /** The key in the OS credential store. `daemon-token` is deliberately not among them. */
  key: string;
  /** The area (settings document name) it belongs to, so it renders beside that document. */
  area: string;
  what: string;
  /**
   * Whether it is stored. `null` means the store could not be asked.
   *
   * Three states and not two, deliberately: "not set" is a fact somebody acts on
   * by pasting a credential, and reporting it for a store that simply did not
   * answer would have them paste one they had already pasted.
   */
  present: boolean | null;
}

export function useMachineSecrets() {
  return useQuery({
    queryKey: keys.system.secrets,
    queryFn: () => apiFetch<{ secrets: MachineSecret[] }>("/config/secrets"),
  });
}

/** Stores one credential — `PUT /config/secrets/{key}`. The value goes up and never comes back. */
export function useStoreSecret() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ key, value }: { key: string; value: string }) =>
      apiFetch<void>(`/config/secrets/${encodeURIComponent(key)}`, {
        method: "PUT",
        body: JSON.stringify({ value }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.secrets });
    },
  });
}

/** Forgets one credential — `DELETE /config/secrets/{key}`, idempotent at the daemon. */
export function useForgetSecret() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (key: string) =>
      apiFetch<void>(`/config/secrets/${encodeURIComponent(key)}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.secrets });
    },
  });
}
