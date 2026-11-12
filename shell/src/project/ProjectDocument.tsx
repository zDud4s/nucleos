import { useEffect, useRef } from "react";
import { useSettingDoc, useSettingsIndex } from "../data/settings";
import { SettingsForm } from "../settings/SettingsForm";

export interface ProjectDocumentProps {
  projectId: string;
  /** The document's name in the project scope: `autopilot` or `workflow_pins`. */
  name: string;
  /** Called when the stored document's version moves, by this form or by anybody else. */
  onChanged?: () => void;
}

/**
 * One document of a project's scope, drawn by `SettingsForm`.
 *
 * Mounted only once the index lists it, so a name the núcleo does not serve is a sentence and not a
 * request that can only 404. If the index itself failed the form mounts anyway: it has its own words
 * for a daemon that does not answer, and two notes saying the same thing would be noise.
 */
export function ProjectDocument({ projectId, name, onChanged }: ProjectDocumentProps) {
  const scope = `project:${projectId}`;
  const index = useSettingsIndex(scope);

  if (index.isPending) return <p className="text-sm text-text-faint">Reading this document…</p>;

  const row = index.data?.find((candidate) => candidate.scope === scope && candidate.name === name);
  if (index.data !== undefined && row === undefined) {
    return <p className="text-sm text-text-muted">This project has no {name} document to edit.</p>;
  }

  return (
    <WatchedForm scope={scope} name={name} takesEffect={row?.takes_effect} onChanged={onChanged} />
  );
}

/**
 * The form, plus a watch on the version it shows.
 *
 * On the version and not on the mutation: it shares the form's query, so it also sees a write made
 * from elsewhere, and a refused save (same version) calls nothing.
 */
function WatchedForm({
  scope,
  name,
  takesEffect,
  onChanged,
}: {
  scope: string;
  name: string;
  takesEffect: string | undefined;
  onChanged: (() => void) | undefined;
}) {
  const doc = useSettingDoc(scope, name);
  const version = doc.data?.version;
  const seen = useRef(version);

  useEffect(() => {
    if (version === undefined) return;
    if (seen.current !== undefined && seen.current !== version) onChanged?.();
    seen.current = version;
  }, [version, onChanged]);

  return <SettingsForm scope={scope} name={name} takesEffect={takesEffect} />;
}
