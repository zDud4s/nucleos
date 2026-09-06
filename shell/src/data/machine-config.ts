import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * This machine's own settings — the nine `.ai/*.yaml` files whose author is the
 * daemon rather than any project.
 *
 * Deliberately its own module and not part of `data/system.ts`, for the reason
 * the núcleo gives for keeping two registries: a project's files and the
 * machine's are two different fences with two different roots, and a page that
 * reached for one when it meant the other would edit a real file with great
 * confidence. `data/project-config.ts` is the other half and stays untouched.
 *
 * The three older readouts (`/config/email`, `/voice/config`, `/calendar/config`)
 * are NOT replaced by this. They serve the daemon's *parsed and running* view —
 * what the pillar is actually doing right now, clamps applied — where this
 * serves the file on disk. Those two answers differ exactly when somebody has
 * edited a file and not restarted, which is the state this page exists to make
 * visible rather than to hide.
 */
export interface MachineSetting {
  /** Relative to the daemon's working directory, forward slashes. The wire identity of the row. */
  path: string;
  /** `email`, `voice`, `calendar`, `web`, `browser`, `telegram`, `github`, `council`, `models`. */
  area: string;
  /** What editing it changes, in the núcleo's own words. Rendered as-is. */
  what: string;
  /**
   * When a write starts mattering, in the núcleo's own words.
   *
   * Rendered as-is and never summarised into a badge: eight of the nine say a
   * restart is needed and the ninth says something genuinely different about
   * its two halves. A boolean here would have to lie about one of them.
   */
  takes_effect: string;
  /** Whether the file is on disk. `false` is "never configured", not "empty". */
  exists: boolean;
  /** The file, verbatim, or `null` when it does not exist. No secret is in any of them. */
  contents: string | null;
  /**
   * The absolute path this row would actually write.
   *
   * Shown, not decorative: this machine has twenty-odd worktrees and every one
   * has an `.ai/`, so the relative path alone would let somebody edit settings
   * in the wrong checkout and believe they had not.
   */
  resolved: string;
}

export interface MachineConfig {
  /** The daemon's working directory — the root every `path` above hangs off. */
  root: string;
  settings: MachineSetting[];
}

/** The fence and what is currently inside it — `GET /config/machine`. */
export function useMachineConfig() {
  return useQuery({
    queryKey: keys.system.machine,
    queryFn: () => apiFetch<MachineConfig>("/config/machine"),
  });
}

export interface MachineWrite {
  path: string;
  contents: string;
}

/**
 * Writes one settings file — `POST /config/machine`.
 *
 * `retry: false`, like every other mutation here: the interesting failures are
 * refusals (`invalid`, `not_ours`, `kill_switch`), and a refusal is settled.
 * Retrying one asks a question that has already been answered.
 *
 * No optimistic update. The daemon validates before it writes, so the answer to
 * "what is in the file now" is the daemon's and not ours to guess — and guessing
 * would show the caller's text as saved in exactly the case where it was not.
 */
export function useWriteMachineSetting() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ path, contents }: MachineWrite) =>
      apiFetch<void>("/config/machine", {
        method: "POST",
        body: JSON.stringify({ path, contents }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.machine });
    },
  });
}

/* ------------------------------------------------------------ credentials -- */

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
  /** The area it belongs to, matching a {@link MachineSetting.area}, so it renders beside its file. */
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
