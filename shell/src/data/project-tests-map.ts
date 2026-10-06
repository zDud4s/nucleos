import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

export interface TestsMapProposal {
  yaml: string;
  sources: string[];
}

/** `GET /projects/{id}/tests-map`: the project's `nucleos.tests.yaml`, and what the daemon proposes. */
export interface TestsMapView {
  state: "absent" | "invalid" | "valid";
  errors: string[];
  groups: string[];
  proposal: TestsMapProposal;
}

export function useProjectTestsMap(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.testsMap(projectId ?? ""),
    queryFn: () =>
      apiFetch<TestsMapView>(`/projects/${encodeURIComponent(projectId ?? "")}/tests-map`),
    enabled: projectId !== null,
  });
}
