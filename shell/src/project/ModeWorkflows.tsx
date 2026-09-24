import { Workflows } from "./Workflows";

/**
 * "How does this get developed?"
 *
 * A workflow is a graph of how work moves through a project, and this núcleo already has one — it
 * is just implicit and spread across three files. `.ai/` holds the sequence in prose, the model
 * assignments in YAML, and the conditional edges in a numbered list of rules ("review runs when
 * Risk is elevated OR Size is medium/large"). It already mixes four kinds of step without ever
 * saying so. This mode does not invent a vocabulary; it names what is there.
 *
 * **What lands first is the socket, not the picture.** A bundle in a library, a pin in the project
 * carrying an origin and a hash, an overlay of what this project changes, and the drift between the
 * two — all of it decided by hashing bytes, so none of it needed the graph's format to be settled.
 * The canvas is drawn on top of this and can invent whatever format it likes without moving a line
 * of it.
 */

export interface ModeWorkflowsProps {
  projectId: string;
}

export function ModeWorkflows({ projectId }: ModeWorkflowsProps) {
  // No wrapper: a flex column around one child spaced nothing, and `Workflows` owns its own rhythm.
  return <Workflows projectId={projectId} />;
}
