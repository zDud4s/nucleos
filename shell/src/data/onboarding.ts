import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { GateProposal, Harness } from "./detect";
import { keys } from "./keys";

/**
 * Bringing a project under NucleOS.
 *
 * What the mode door asks of a project, besides the classifier hook, is that a person onboarded it:
 * looked at what is in the folder, confirmed the command that decides *green*, and had the hook
 * installed. The daemon records that as a marker in the project's state directory; whether the
 * project uses any particular way of working is none of this app's business.
 */

/** The marker, as the daemon recorded it. */
export interface OnboardingMarker {
  onboarded_at: string;
  project_root: string;
  gate_command: string | null;
  harnesses: string[];
  hook_installed: boolean;
  /** Written at startup for a project that passed the old check, rather than by a person. */
  migrated: boolean;
}

/** `GET` and `POST /projects/{id}/onboard` both answer this. */
export interface Onboarding {
  project_id: string;
  root: string;
  onboarded: OnboardingMarker | null;
  /** Where the marker lives, as a person is shown it. */
  marker_path: string;
  harnesses: Harness[];
  proposed_gate: GateProposal | null;
  /** The gate the project's rules already name — the field starts from this when there is one. */
  configured_gate: string | null;
  hook_wired: boolean;
}

/**
 * What onboarding would find and propose.
 *
 * `root` is the folder to read when the project has none on record (one being added, or one in
 * `off`); empty means the root on record. `retry: false`: every refusal here is a fact about the
 * folder or the id, and a second ask gets the same answer.
 */
export function useOnboarding(projectId: string, root: string, enabled = true) {
  const query = root.trim() === "" ? "" : `?root=${encodeURIComponent(root.trim())}`;
  return useQuery({
    queryKey: keys.projects.onboarding(projectId, root.trim()),
    queryFn: () =>
      apiFetch<Onboarding>(`/projects/${encodeURIComponent(projectId)}/onboard${query}`),
    enabled: enabled && projectId !== "",
    retry: false,
    refetchOnWindowFocus: false,
  });
}

/** The confirmation. `gate_command` blank or null is "none confirmed", which changes no rule. */
export function useOnboard() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      projectId,
      projectRoot,
      gateCommand,
    }: {
      projectId: string;
      projectRoot: string | null;
      gateCommand: string | null;
    }) =>
      apiFetch<Onboarding>(`/projects/${encodeURIComponent(projectId)}/onboard`, {
        method: "POST",
        body: JSON.stringify({
          ...(projectRoot === null || projectRoot.trim() === ""
            ? {}
            : { project_root: projectRoot.trim() }),
          gate_command: gateCommand === null || gateCommand.trim() === "" ? null : gateCommand.trim(),
        }),
      }),
    retry: false,
    onSettled: (_data, _error, { projectId }) => {
      void queryClient.invalidateQueries({ queryKey: ["projects", projectId] });
    },
  });
}
