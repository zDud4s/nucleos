import { useProjectMap } from "../data/project-map";
import { buildMap } from "../canvas/map-model";
import { ExtrairSpec } from "./ExtrairSpec";
import { MapaPorAprovar } from "./MapaPorAprovar";

/**
 * "What is in here, what did nobody ask for, and what did this project actually decide?"
 *
 * Two of the three layers are here now: the structure, derived off disk and always true, and the
 * intention — a model reading one spec and proposing the decisions it fixes, which nothing enters
 * the map without the owner answering line by line. Evidence and stamps are not, and the mode says
 * so in words rather than showing a map that looks complete and is not. A surface that implies it
 * has layers it does not have is the false confidence again, with better pixels.
 *
 * **The three panels below read three different questions, and one failing does not silence the
 * others.** The structure is derived by walking the project's folder; the pile is a table the
 * daemon answers without touching disk at all — `get_project_map_decisions` resolves the row and
 * never the folder, deliberately, "so a project whose folder has moved still has a pile, which is
 * exactly what somebody looking at a broken project wants". A mode that returned a single sentence
 * on the first failure would take that away for no reason.
 */

export interface ModeMapaProps {
  projectId: string;
}

export function ModeMapa({ projectId }: ModeMapaProps) {
  return (
    <div className="flex flex-col gap-8">
      <Structure projectId={projectId} />
      <ExtrairSpec projectId={projectId} />
      <MapaPorAprovar projectId={projectId} />
      <p className="text-sm text-text-muted">
        Structure and intention. Evidence and stamps are slices that do not exist yet, so nothing
        here yet says whether the code keeps what you approved.
      </p>
    </div>
  );
}

/** The structure layer: what modules exist, and how many of them declare nothing they implement. */
function Structure({ projectId }: { projectId: string }) {
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
  );
}
