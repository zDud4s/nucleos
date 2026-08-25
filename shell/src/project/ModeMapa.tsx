import { useProjectMap } from "../data/project-map";
import { buildMap } from "../canvas/map-model";

/**
 * "What is in here, and what did nobody ask for?"
 *
 * This slice draws the structure layer only. There are no stamps, no decisions and no triage
 * — and the mode says so in words rather than showing a map that looks complete and is not.
 * A surface that implies it has layers it does not have is the false confidence again, with
 * better pixels.
 */

export interface ModeMapaProps {
  projectId: string;
}

export function ModeMapa({ projectId }: ModeMapaProps) {
  const map = useProjectMap(projectId);

  if (map.isError) {
    return (
      <p className="text-sm text-text-faint">
        The núcleo could not read this project&rsquo;s map — its folder may have moved.
      </p>
    );
  }
  if (map.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the project&rsquo;s tree…</p>;
  }

  const { modules, imports, unread } = map.data;
  // `buildMap` rather than `imports.length`, and the difference is the whole point: this counts
  // the links the map would actually draw, which drops any edge with an end it cannot find. The
  // núcleo sends none of those today, and the day it does this number must not quietly start
  // counting things nobody will ever see.
  const built = buildMap(modules, imports);
  const undeclared = modules.filter((module) => !module.declares).length;

  return (
    <div className="flex flex-col gap-8">
      <div>
        <p className="mt-1 font-display text-3xl text-text">
          {undeclared}
          <span className="ml-2 text-sm text-text-faint">
            module{undeclared === 1 ? "" : "s"} declaring nothing they implement
          </span>
        </p>
        <p className="mt-1 text-xs text-text-muted">
          out of {modules.length} this reader could read, joined by {built.edges.length} link
          {built.edges.length === 1 ? "" : "s"}
          {unread.length > 0
            ? ` · ${unread.length} file${unread.length === 1 ? "" : "s"} in a language it cannot read yet`
            : ""}
        </p>
      </div>
      <p className="text-sm text-text-muted">
        The structure layer only. Intention, evidence and stamps are slices that do not exist yet.
      </p>
    </div>
  );
}
