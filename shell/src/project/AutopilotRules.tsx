import { ProjectDocument } from "./ProjectDocument";

/** The project's `autopilot` document: what it does on its own, and the gate that decides green. */
export function AutopilotRules({ projectId }: { projectId: string }) {
  return <ProjectDocument projectId={projectId} name="autopilot" />;
}
